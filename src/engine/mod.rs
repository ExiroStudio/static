//! The core engine: a minimal realtime signal runtime.
//!
//! The engine owns the GPU context, the webcam *source*, and the live, editable
//! [`PipelineConfig`] (the single source of truth, mirroring `pipeline.json`).
//! It drives a [`PipelineRuntime`]:
//!
//! ```text
//!   webcam (source) ──▶ pipeline nodes ──▶ window (sink)
//! ```
//!
//! The UI never touches the runtime, the registry internals, or any node type
//! ([`FilterNode`](crate::runtime::FilterNode), `FrameContext`, …). It edits
//! the config through the thin API on this struct (`set_param`, `set_enabled`,
//! …) and reads it back through [`config`](Engine::config) /
//! [`registry`](Engine::registry). Edits are applied to the running pipeline by
//! [`tick_reload`](Engine::tick_reload) (see [`reload`]).

pub mod gpu;
pub mod image;
mod reload;
pub mod video;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use wgpu::*;
use winit::window::Window;

use crate::addon::package;
use crate::addon::pipeline::{PipelineConfig, SinkConfig, SourceConfig};
use crate::addon::registry::AddonRegistry;
use crate::addon::schema::ParamValue;
use crate::addons::{CrtAddon, DotRendererAddon};
use crate::behavior::BehaviorRuntime;
use crate::camera::WebcamCapture;
use crate::runtime::{PipelineRuntime, SINK_WINDOW, SOURCE_WEBCAM};
use crate::signal::{SignalReader, SignalSchema, SignalSnapshot, SignalStore};

use gpu::GpuContext;
use reload::ReloadState;
use video::VideoTexture;

/// Where the engine looks for the pipeline definition, relative to the working
/// directory. Missing or invalid → the built-in default pipeline is used.
const PIPELINE_PATH: &str = "pipeline.json";

/// Directory holding installed (on-disk) addons, one subdirectory per addon id.
const ADDONS_ROOT: &str = "addons";

pub struct Engine {
    gpu: GpuContext,

    /// The webcam source: frames stream into this texture every frame.
    video: VideoTexture,

    runtime: PipelineRuntime,

    /// The live, editable document. The UI mutates this; `tick_reload` applies
    /// it to `runtime`.
    config: PipelineConfig,
    /// The last config that successfully built — what is actually running. A
    /// rejected edit keeps this live.
    last_good: PipelineConfig,
    reload: ReloadState,

    /// The consumer end of the signal store; the producer end lives on the
    /// behavior thread. Read once per frame into `signals`.
    reader: SignalReader,
    /// The reusable per-frame snapshot buffer (allocated once).
    signals: SignalSnapshot,
    /// The behavior thread. Held for its lifetime; dropping it stops + joins.
    _behavior: BehaviorRuntime,

    frame_buf: Vec<u8>,
    start: Instant,

    // ---- spike metrics (logged once per second) ----
    frames: u32,
    last_stat: Instant,
    last_pubs: u64,
}

impl Engine {
    pub async fn new(window: Arc<Window>, cam_width: u32, cam_height: u32) -> Self {
        let gpu = GpuContext::new(window).await;

        let video = VideoTexture::new(&gpu.device, cam_width, cam_height);

        let mut runtime = PipelineRuntime::new(
            &gpu.device,
            gpu.config.format,
            gpu.config.width,
            gpu.config.height,
        );

        // Register builtin addons through the same path an external addon would
        // use. The runtime does not know what these *are* — only that they exist.
        runtime
            .register_builtin::<DotRendererAddon>()
            .expect("register dot-renderer");
        runtime
            .register_builtin::<CrtAddon>()
            .expect("register crt");

        // The webcam is the source; hand its view to the runtime.
        runtime.set_source(&gpu.device, &video.view);

        // Pick up any on-disk addons installed under `addons/` (missing → no-op).
        if let Err(e) = runtime.scan_addons(Path::new(ADDONS_ROOT)) {
            eprintln!("[engine] addon scan failed: {e}");
        }

        // Load the pipeline definition, validate it against the registry, and
        // build the runtime pipeline — all before the first frame renders.
        let config = load_pipeline_config();
        runtime
            .build(&gpu.device, &config)
            .expect("failed to build pipeline from pipeline.json");

        // Signal store + behavior thread. The behavior thread publishes
        // `signal.time`; the CRT filter consumes it. They share only the store.
        let (publisher, reader) = SignalStore::new(&SignalSchema::standard());
        let signals = reader.snapshot();
        let behavior = BehaviorRuntime::spawn(publisher);

        Self {
            gpu,
            video,
            runtime,
            last_good: config.clone(),
            config,
            reload: ReloadState::new(),
            reader,
            signals,
            _behavior: behavior,
            frame_buf: Vec::new(),
            start: Instant::now(),
            frames: 0,
            last_stat: Instant::now(),
            last_pubs: 0,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.gpu.resize(width, height);
        self.runtime
            .resize(&self.gpu.device, self.gpu.config.width, self.gpu.config.height);
    }

    // ---- UI-facing read API (no engine internals leak out) ----

    /// The live, editable pipeline document.
    pub fn config(&self) -> &PipelineConfig {
        &self.config
    }

    /// Read-only listing of installed addons (for the addon list + resolving
    /// display names and param schemas in the properties panel).
    pub fn registry(&self) -> &AddonRegistry {
        self.runtime.registry()
    }

    /// The GPU device — needed by the UI layer to construct its egui renderer.
    pub fn device(&self) -> &Device {
        &self.gpu.device
    }

    /// The swapchain texture format the UI must paint into.
    pub fn surface_format(&self) -> TextureFormat {
        self.gpu.config.format
    }

    /// The last reload error in plain language, if the current `config` failed
    /// to apply (the previous pipeline stays live in that case).
    pub fn last_error(&self) -> Option<&str> {
        self.reload.last_error()
    }

    // ---- UI-facing edit API (mutate the document, schedule a rebuild) ----

    /// Set one parameter on a node. Continuous edit → debounced apply.
    pub fn set_param(&mut self, instance_id: &str, key: &str, value: ParamValue) {
        if self.config.set_param(instance_id, key, value) {
            self.reload.mark_dirty();
        }
    }

    /// Enable/disable a node. Discrete edit → apply next frame.
    pub fn set_enabled(&mut self, instance_id: &str, enabled: bool) {
        if self.config.set_enabled(instance_id, enabled) {
            self.reload.mark_dirty_now();
        }
    }

    /// Append an addon to the pipeline. Returns the new node's `instance_id`.
    pub fn add_node(&mut self, addon_id: &str) -> String {
        let instance_id = self.config.add_node(addon_id, None);
        self.reload.mark_dirty_now();
        instance_id
    }

    /// Remove a node from the pipeline (does not touch the addon on disk).
    pub fn remove_node(&mut self, instance_id: &str) {
        if self.config.remove_node(instance_id) {
            self.reload.mark_dirty_now();
        }
    }

    /// Move a node to position `to` (reorder). Preserves the node's id + config.
    pub fn move_node(&mut self, instance_id: &str, to: usize) {
        if self.config.move_node(instance_id, to) {
            self.reload.mark_dirty_now();
        }
    }

    /// Whether the runtime can actually run `addon_id` (has an implementation).
    /// Installed-but-not-runnable addons are listed but can't render.
    pub fn is_runnable(&self, addon_id: &str) -> bool {
        self.runtime.has_implementation(addon_id)
    }

    /// Whether `addon_id` is referenced by any node in the current pipeline.
    pub fn addon_in_use(&self, addon_id: &str) -> bool {
        self.config.pipeline.iter().any(|n| n.addon == addon_id)
    }

    /// Install an addon from a ZIP package: extract, validate, rescan the
    /// registry, schedule a rebuild. Returns a user-facing message on either
    /// outcome (the live pipeline keeps running regardless).
    pub fn install_addon(&mut self, zip_path: &Path) -> std::result::Result<String, String> {
        match self.try_install(zip_path) {
            Ok(id) => Ok(format!("Installed “{id}”.")),
            Err(e) => Err(reload::humanize(&e)),
        }
    }

    fn try_install(&mut self, zip_path: &Path) -> crate::addon::Result<String> {
        let installed = package::install(zip_path, Path::new(ADDONS_ROOT))?;
        self.runtime.rescan_addons(Path::new(ADDONS_ROOT))?;
        // Registry changed; re-validate / rebuild (pipeline itself is unchanged).
        self.reload.mark_dirty_now();
        let id = installed
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("addon")
            .to_string();
        Ok(id)
    }

    /// Uninstall an addon: first remove any pipeline nodes using it (so the
    /// pipeline never silently breaks), then delete its directory and rescan.
    pub fn uninstall_addon(&mut self, addon_id: &str) -> std::result::Result<String, String> {
        self.config.pipeline.retain(|n| n.addon != addon_id);
        match package::uninstall(Path::new(ADDONS_ROOT), addon_id)
            .and_then(|()| self.runtime.rescan_addons(Path::new(ADDONS_ROOT)))
        {
            Ok(()) => {
                self.reload.mark_dirty_now();
                Ok(format!("Uninstalled “{addon_id}”."))
            }
            Err(e) => Err(reload::humanize(&e)),
        }
    }

    /// Apply the working config to the running pipeline once edits have settled.
    /// On success the new config becomes the running one and is persisted; on
    /// failure the previous pipeline keeps running and the error is surfaced.
    pub fn tick_reload(&mut self) {
        if !self.reload.take_if_settled() {
            return;
        }
        match self.runtime.build(&self.gpu.device, &self.config) {
            Ok(()) => {
                self.last_good = self.config.clone();
                self.reload.set_error(None);
                if let Err(e) = self.last_good.save(Path::new(PIPELINE_PATH)) {
                    self.reload
                        .set_error(Some(format!("Applied, but couldn't save: {e}")));
                }
            }
            Err(e) => {
                // The live pipeline is still `last_good`; just report.
                self.reload.set_error(Some(reload::humanize(&e)));
            }
        }
    }

    // ---- per-frame rendering ----

    /// Render the live pipeline to the surface, then invoke `overlay` to paint
    /// on top of the same surface frame (the egui UI) before presenting. The
    /// overlay must use `LoadOp::Load` so it composites over the preview.
    pub fn render_with_overlay(
        &mut self,
        camera: &WebcamCapture,
        overlay: impl FnOnce(&Device, &Queue, &TextureView, [u32; 2]),
    ) {
        // Stream the newest webcam frame to the GPU (the source).
        if camera.copy_latest(&mut self.frame_buf) && !self.frame_buf.is_empty() {
            self.video.upload(&self.gpu.queue, &self.frame_buf);
        }

        let frame = match self.gpu.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                self.gpu.surface.configure(&self.gpu.device, &self.gpu.config);
                return; // overlay skipped this frame too; next frame re-runs the UI
            }
            Err(_) => return,
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());

        // One immutable, consistent snapshot per frame; every node reads from it.
        self.reader.snapshot_into(&mut self.signals);

        let time = self.start.elapsed().as_secs_f32();
        self.runtime
            .render(&self.gpu.device, &self.gpu.queue, &view, time, &self.signals); // encoder #1 (submits)

        self.log_stats();

        overlay(
            &self.gpu.device,
            &self.gpu.queue,
            &view,
            [self.gpu.config.width, self.gpu.config.height],
        ); // encoder #2 (egui), same surface frame

        frame.present();
    }

    /// Spike instrumentation: once per second, report FPS, rebuild count, signal
    /// publish frequency, and the current `signal.time`. The invariant to watch
    /// is `builds` staying constant while `signal.time` oscillates.
    fn log_stats(&mut self) {
        self.frames += 1;
        let dt = self.last_stat.elapsed().as_secs_f32();
        if dt < 1.0 {
            return;
        }
        let fps = self.frames as f32 / dt;
        let pubs = self.reader.published();
        let signal_hz = (pubs - self.last_pubs) as f32 / dt;
        let builds = self.runtime.build_count();
        let t = SignalSchema::standard()
            .id("signal.time")
            .and_then(|id| self.signals.get(id).as_f32())
            .unwrap_or(0.0);
        eprintln!(
            "[spike] fps={fps:.1} builds={builds} signal_hz={signal_hz:.0} signal.time={t:+.3}"
        );
        self.frames = 0;
        self.last_pubs = pubs;
        self.last_stat = Instant::now();
    }
}

/// Load `pipeline.json`, falling back to the built-in default pipeline if it is
/// missing or unreadable. Structural problems in an existing file are surfaced
/// (panic) rather than silently swallowed, since the user clearly intended to
/// drive the pipeline from disk.
fn load_pipeline_config() -> PipelineConfig {
    let path = Path::new(PIPELINE_PATH);
    if path.exists() {
        match PipelineConfig::load(path) {
            Ok(config) => {
                println!("[engine] loaded pipeline from {PIPELINE_PATH}");
                return config;
            }
            Err(e) => panic!("[engine] failed to load {PIPELINE_PATH}: {e}"),
        }
    }
    println!("[engine] no {PIPELINE_PATH} found — using default pipeline");
    default_pipeline()
}

/// The default pipeline: webcam → dot-renderer → crt → window.
fn default_pipeline() -> PipelineConfig {
    let mut config = PipelineConfig::new(
        SourceConfig {
            kind: SOURCE_WEBCAM.into(),
            config: serde_json::Value::Object(Default::default()),
        },
        SinkConfig {
            kind: SINK_WINDOW.into(),
            config: serde_json::Value::Object(Default::default()),
        },
    );
    config.add_node("dot-renderer", None);
    config.add_node("crt", None);
    config
}
