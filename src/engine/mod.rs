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
use std::time::{Duration, Instant};

use wgpu::*;
use winit::window::Window;

use crate::addon::package;
use crate::addon::pipeline::{PipelineConfig, SinkConfig, SourceConfig};
use crate::addon::registry::AddonRegistry;
use crate::addon::schema::ParamValue;
use crate::behavior::{BehaviorHandle, BehaviorHost, BehaviorInit, BehaviorRuntime};
use crate::camera::{FrameSource, WebcamCapture};
use crate::runtime::{PipelineRuntime, SINK_WINDOW, SOURCE_WEBCAM};
use crate::signal::{
    SchemaError, SignalReader, SignalSchema, SignalSchemaBuilder, SignalSnapshot, SignalStore,
};

use gpu::GpuContext;
use reload::ReloadState;
use video::VideoTexture;

/// Where the engine looks for the pipeline definition, relative to the working
/// directory. Missing or invalid → the built-in default pipeline is used.
const PIPELINE_PATH: &str = "pipeline.json";

/// Directory holding installed (on-disk) addons, one subdirectory per addon id.
const ADDONS_ROOT: &str = "addons";

pub struct Engine {
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

    /// The signal schema for the current build (publishers ∪ consumers). Owned
    /// by the render thread; an `Arc` clone is shared with the behavior thread.
    schema: Arc<SignalSchema>,
    /// The consumer end of the signal store; the producer end lives on the
    /// behavior thread. Read once per frame into `signals`.
    reader: SignalReader,
    /// The reusable per-frame snapshot buffer (re-allocated only when the schema
    /// changes).
    signals: SignalSnapshot,
    /// Control handle for the behavior thread. Dropping it stops + joins.
    behavior: BehaviorHandle,
    /// Wall time of the last structural rebuild (diagnostics).
    last_reload_ms: f32,
    /// Total `@group(3)` uniform bytes across the live filters (diagnostics).
    group3_bytes: usize,

    frame_buf: Vec<u8>,
    start: Instant,

    // ---- metrics (sampled once per second; exposed via `stats()`) ----
    frames: u32,
    last_stat: Instant,
    last_pubs: u64,
    /// Explicit epoch tracked for signal metrics. Incremented on every
    /// structural reload (SignalStore swap). Prevents implicit reset logic
    /// from misinterpreting a standard counter overflow as a reload.
    metric_epoch: u64,
    last_metric_epoch: u64,
    cur_fps: f32,
    cur_signal_hz: f32,
    /// Pending debounced config save for hot (non-rebuild) behavior edits.
    save_at: Option<Instant>,

    /// The GPU context. Moved to the bottom of the struct to ensure it is the
    /// LAST field dropped during unwinding/destruction, avoiding Surface
    /// teardown while stack-resident SurfaceTextures are still alive.
    gpu: GpuContext,
}

/// Debounce for persisting hot behavior edits (matches the reload debounce).
const SAVE_DEBOUNCE: Duration = Duration::from_millis(120);

/// A snapshot of live engine + behavior metrics (for the UI / diagnostics).
#[derive(Clone, Copy, Debug, Default)]
pub struct EngineStats {
    pub fps: f32,
    pub build_count: u64,
    pub signal_hz: f32,
    pub behavior_hz: f32,
    pub behavior_update_us: f32,
    pub reload_ms: f32,
    pub group3_bytes: usize,
}

impl Engine {
    pub async fn new(
        window: Arc<Window>,
        cam_width: u32,
        cam_height: u32,
        frame: FrameSource,
    ) -> Self {
        let gpu = GpuContext::new(window).await;

        let video = VideoTexture::new(&gpu.device, cam_width, cam_height);

        let mut runtime = PipelineRuntime::new(
            &gpu.device,
            gpu.config.format,
            gpu.config.width,
            gpu.config.height,
        );

        // The webcam is the source; hand its view to the runtime.
        runtime.set_source(&gpu.device, &video.view);

        // Pick up any on-disk addons installed under `addons/` (missing → no-op).
        // Scanned *before* behavior registration so an executable behavior can
        // bind its factory to a package discovered here (the package then owns
        // the UI param schema; the factory supplies only execution).
        if let Err(e) = runtime.scan_addons(Path::new(ADDONS_ROOT)) {
            eprintln!("[engine] addon scan failed: {e}");
        }


        let mut config = load_pipeline_config();

        // [R3] Robustness: filter out any reference to an addon that doesn't exist
        // in the registry. This prevents startup panics when an addon is uninstalled.
        if Self::clean_config(&mut config, &runtime) {
            println!("[engine] pipeline.json contained missing addons; cleaning...");
            let _ = config.save(Path::new(PIPELINE_PATH));
        }

        // Build the schema from the config's behaviors (publish) + filters
        // (consume), then the store + filters — all before the first frame.
        let (inits, skipped) = BehaviorHost::create_inits(runtime.behavior_registry(), &config.behaviors);
        for (id, reason) in skipped {
            eprintln!("[engine] behavior addon {id:?} skipped: {reason:?}");
        }

        let (schema, warnings) = build_schema(&inits, &config, runtime.registry())
            .unwrap_or_else(|e| {
                eprintln!("[engine] schema build failed: {e}. Falling back to empty schema.");
                (Arc::new(SignalSchema::default()), vec![])
            });

        for w in &warnings {
            eprintln!("[engine] {w}");
        }

        if let Err(e) = runtime.build(&gpu.device, &config, &schema) {
            eprintln!("[engine] failed to build pipeline: {e}. Running with empty pipeline.");
            // Fallback: build an empty pipeline config to at least start the window.
            let fallback = PipelineConfig::new(config.source.clone(), config.sink.clone());
            let _ = runtime.build(&gpu.device, &fallback, &schema);
        }

        // Signal store + behavior thread. The behavior thread publishes;
        // the filters consume. They share only the store.
        let (publisher, reader) = SignalStore::new(&schema);
        let signals = reader.snapshot();
        let group3_bytes = group3_bytes(&config, runtime.registry());
        let behavior = BehaviorRuntime::spawn(publisher, schema.clone(), frame, inits);

        Self {
            gpu,
            video,
            runtime,
            last_good: config.clone(),
            config,
            reload: ReloadState::new(),
            schema,
            reader,
            signals,
            behavior,
            last_reload_ms: 0.0,
            group3_bytes,
            frame_buf: Vec::new(),
            start: Instant::now(),
            frames: 0,
            last_stat: Instant::now(),
            last_pubs: 0,
            metric_epoch: 0,
            last_metric_epoch: 0,
            cur_fps: 0.0,
            cur_signal_hz: 0.0,
            save_at: None,
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
        // Step 1: Stop any nodes or behaviors using this addon.
        self.config.pipeline.retain(|n| n.addon != addon_id);
        self.config.behaviors.retain(|b| b.addon != addon_id);

        // Step 2: Synchronous rebuild. This tells the behavior thread to stop the
        // worker and the render thread to drop the nodes. No new frames will touch
        // the addon's assets after this.
        self.rebuild(true)?;

        // Step 3: Delete files only after execution has been safely drained.
        match package::uninstall(Path::new(ADDONS_ROOT), addon_id)
            .and_then(|()| self.runtime.rescan_addons(Path::new(ADDONS_ROOT)))
        {
            Ok(()) => {
                // Mark dirty just in case, though the sync rebuild already applied changes.
                self.reload.mark_dirty_now();
                Ok(format!("Uninstalled “{addon_id}”."))
            }
            Err(e) => Err(reload::humanize(&e)),
        }
    }

    /// Apply the working config once edits have settled. On success the new
    /// config becomes the running one and is persisted; on failure the previous
    /// build keeps running and the error is surfaced. The render thread never
    /// waits on the behavior thread — its reload is fire-and-forget.
    pub fn tick_reload(&mut self) {
        if self.reload.take_if_settled() {
            let t0 = Instant::now();
            match self.rebuild(false) {
                Ok(()) => {
                    self.last_reload_ms = t0.elapsed().as_secs_f32() * 1000.0;
                    self.reload.set_error(None);
                    self.persist();
                }
                Err(e) => {
                    // Nothing was swapped; the live build is still `last_good`.
                    self.reload.set_error(Some(e));
                }
            }
        }
        // Persist hot (non-rebuild) behavior edits once they settle.
        if matches!(self.save_at, Some(t) if t.elapsed() >= SAVE_DEBOUNCE) {
            self.save_at = None;
            self.persist();
        }
    }

    /// Persist the working config and remember it as the last-good one.
    fn persist(&mut self) {
        self.last_good = self.config.clone();
        if let Err(e) = self.last_good.save(Path::new(PIPELINE_PATH)) {
            self.reload
                .set_error(Some(format!("Applied, but couldn't save: {e}")));
        }
    }

    /// Build schema → filters from the working config. The signal store and
    /// behavior thread are only re-created when the schema's *signals* change
    /// (i.e. a behavior was added/removed) — filter edits and behavior
    /// enable/param edits leave them running. On any error nothing is swapped.
    fn rebuild(&mut self, sync: bool) -> std::result::Result<(), String> {
        // Filter out any nodes that no longer exist in the registry (e.g. uninstalled).
        if Self::clean_config(&mut self.config, &self.runtime) {
            println!("[engine] cleaning uninstalled addons from config...");
            let _ = self.config.save(Path::new(PIPELINE_PATH));
        }

        let (inits, skipped) =
            BehaviorHost::create_inits(self.runtime.behavior_registry(), &self.config.behaviors);
        for (id, reason) in skipped {
            eprintln!("[engine] behavior addon {id:?} skipped: {reason:?}");
        }

        let (schema, warnings) =
            match build_schema(&inits, &self.config, self.runtime.registry()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[engine] rebuild schema error: {e}");
                    return Err(e.to_string());
                }
            };
        for w in &warnings {
            eprintln!("[engine] {w}");
        }
        let schema_changed = schema.as_ref() != self.schema.as_ref();

        // Build filters against whichever schema will be live; on failure keep
        // the live build (any new store endpoints are simply dropped).
        if schema_changed {
            let (publisher, reader) = SignalStore::new(&schema);
            self.runtime
                .build(&self.gpu.device, &self.config, &schema)
                .map_err(|e| reload::humanize(&e))?;
            // Success → swap render-side state, async-reload the behavior thread.
            self.schema = schema.clone();
            self.reader = reader;
            self.signals = self.reader.snapshot();
            self.last_pubs = 0;
            self.metric_epoch += 1;
            self.behavior.reload(publisher, schema, inits, sync)
                .map_err(|e| e.to_string())?;
        } else {
            self.runtime
                .build(&self.gpu.device, &self.config, &self.schema)
                .map_err(|e| reload::humanize(&e))?;
        }
        self.group3_bytes = group3_bytes(&self.config, self.runtime.registry());
        Ok(())
    }

    // ---- UI-facing behavior edit API (unordered set) ----

    /// Add a behavior addon to the set. Returns the new instance id.
    pub fn add_behavior(&mut self, addon_id: &str) -> String {
        let id = self.config.add_behavior(addon_id);
        self.reload.mark_dirty_now();
        id
    }

    pub fn remove_behavior(&mut self, instance_id: &str) {
        // Removing a behavior changes the published-signal set → structural.
        if self.config.remove_behavior(instance_id) {
            self.reload.mark_dirty_now();
        }
    }

    /// Enable/disable a behavior. Hot: the schema is unchanged (every behavior's
    /// signals stay declared), so this is a live command, not a rebuild.
    pub fn set_behavior_enabled(&mut self, instance_id: &str, enabled: bool) {
        if self.config.set_behavior_enabled(instance_id, enabled) {
            self.behavior.set_enabled(instance_id, enabled);
            self.save_at = Some(Instant::now());
        }
    }

    /// Set a behavior parameter. Hot: applied live via a command, persisted on
    /// a debounce — no rebuild, no behavior re-create.
    pub fn set_behavior_param(&mut self, instance_id: &str, key: &str, value: ParamValue) {
        if self.config.set_behavior_param(instance_id, key, value.clone()) {
            self.behavior.set_param(instance_id, key, value);
            self.save_at = Some(Instant::now());
        }
    }

    // ---- UI-facing signal inspection (read-only) ----

    /// The live signal schema (names/kinds/ids) for the inspector.
    pub fn signal_schema(&self) -> &SignalSchema {
        &self.schema
    }

    /// The latest per-frame signal snapshot for the inspector.
    pub fn signal_snapshot(&self) -> &SignalSnapshot {
        &self.signals
    }

    /// A snapshot of the live engine + behavior metrics.
    pub fn stats(&self) -> EngineStats {
        let b = self.behavior.stats();
        EngineStats {
            fps: self.cur_fps,
            build_count: self.runtime.build_count(),
            signal_hz: self.cur_signal_hz,
            behavior_hz: b.fps,
            behavior_update_us: b.last_update_us,
            reload_ms: self.last_reload_ms,
            group3_bytes: self.group3_bytes,
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
        let pubs = self.reader.published();
        self.cur_fps = self.frames as f32 / dt;
        if self.metric_epoch == self.last_metric_epoch && pubs >= self.last_pubs {
            self.cur_signal_hz = (pubs - self.last_pubs) as f32 / dt;
        } else {
            // Signal store was swapped OR counter wrapped; use absolute for this frame.
            self.cur_signal_hz = pubs as f32 / dt;
            self.last_metric_epoch = self.metric_epoch;
        }

        // Diagnostics print is debug-only — release builds carry no logging I/O.
        #[cfg(debug_assertions)]
        {
            let s = self.stats();
            let t = self
                .schema
                .id("signal.time")
                .and_then(|id| self.signals.get(id).as_f32())
                .unwrap_or(0.0);
            eprintln!(
                "[stats] fps={:.1} builds={} signal_hz={:.0} signal.time={t:+.3} | \
                 behavior {:.0}Hz update={:.0}us reload={:.1}ms group3={}B",
                s.fps, s.build_count, s.signal_hz, s.behavior_hz, s.behavior_update_us,
                s.reload_ms, s.group3_bytes
            );
        }

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

/// The default pipeline: empty chain.
fn default_pipeline() -> PipelineConfig {
    PipelineConfig::new(
        SourceConfig {
            kind: SOURCE_WEBCAM.into(),
            config: serde_json::Value::Object(Default::default()),
        },
        SinkConfig {
            kind: SINK_WINDOW.into(),
            config: serde_json::Value::Object(Default::default()),
        },
    )
}

impl Engine {
    /// Remove any nodes (behaviors or filters) whose addon ID is not present in
    /// the registry. Returns true if anything was removed.
    fn clean_config(config: &mut PipelineConfig, runtime: &PipelineRuntime) -> bool {
        let mut dirty = false;

        // Clean behaviors
        let b_len = config.behaviors.len();
        config.behaviors.retain(|node| {
            runtime.behavior_registry().get(&node.addon).is_some()
        });
        if config.behaviors.len() != b_len {
            dirty = true;
        }

        // Clean filters
        let p_len = config.pipeline.len();
        config.pipeline.retain(|node| {
            runtime.registry().get(&node.addon).is_some()
        });
        if config.pipeline.len() != p_len {
            dirty = true;
        }

        dirty
    }
}

/// Build the signal schema: every behavior's published signals (so the schema
/// is stable across enable/disable), validated against the enabled filters'
/// consumed signals.
fn build_schema(
    inits: &[BehaviorInit],
    config: &PipelineConfig,
    registry: &AddonRegistry,
) -> std::result::Result<(Arc<SignalSchema>, Vec<String>), SchemaError> {
    let mut builder = SignalSchemaBuilder::new();
    for init in inits {
        builder.publish_all(&init.publish)?;
    }
    for node in config.pipeline.iter().filter(|n| n.enabled) {
        if let Some(entry) = registry.get(&node.addon) {
            builder.validate_consumer(&entry.manifest.consume)?;
        }
    }
    Ok(builder.finish())
}

/// Total `@group(3)` uniform bytes across the enabled filters (16 per consumed
/// signal) — a diagnostics metric.
fn group3_bytes(config: &PipelineConfig, registry: &AddonRegistry) -> usize {
    config
        .pipeline
        .iter()
        .filter(|n| n.enabled)
        .filter_map(|n| registry.get(&n.addon))
        .map(|e| e.manifest.consume.len() * 16)
        .sum()
}
