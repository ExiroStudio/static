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
//! ([`PipelineNode`](crate::runtime::PipelineNode), `FrameContext`, …). It edits
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

use crate::addon::pipeline::{PipelineConfig, SinkConfig, SourceConfig};
use crate::addon::registry::AddonRegistry;
use crate::addon::schema::ParamValue;
use crate::addons::{CrtAddon, DotRendererAddon};
use crate::camera::WebcamCapture;
use crate::runtime::{PipelineRuntime, SINK_WINDOW, SOURCE_WEBCAM};

use gpu::GpuContext;
use reload::ReloadState;
use video::VideoTexture;

/// Where the engine looks for the pipeline definition, relative to the working
/// directory. Missing or invalid → the built-in default pipeline is used.
const PIPELINE_PATH: &str = "pipeline.json";

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

    frame_buf: Vec<u8>,
    start: Instant,
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

        // Load the pipeline definition, validate it against the registry, and
        // build the runtime pipeline — all before the first frame renders.
        let config = load_pipeline_config();
        runtime
            .build(&gpu.device, &config)
            .expect("failed to build pipeline from pipeline.json");

        Self {
            gpu,
            video,
            runtime,
            last_good: config.clone(),
            config,
            reload: ReloadState::new(),
            frame_buf: Vec::new(),
            start: Instant::now(),
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

        let time = self.start.elapsed().as_secs_f32();
        self.runtime
            .render(&self.gpu.device, &self.gpu.queue, &view, time); // encoder #1 (submits)

        overlay(
            &self.gpu.device,
            &self.gpu.queue,
            &view,
            [self.gpu.config.width, self.gpu.config.height],
        ); // encoder #2 (egui), same surface frame

        frame.present();
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
