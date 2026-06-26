//! Pipeline runtime — the missing execution piece of the addon ecosystem.
//!
//! Turns a [`PipelineConfig`] (loaded from `pipeline.json`) into a running
//! render chain:
//!
//! ```text
//!   source ──▶ node ──▶ node ──▶ … ──▶ sink
//! ```
//!
//! The runtime resolves each pipeline node's addon through the
//! [`AddonRegistry`], instantiates it through the addon's own factory, and
//! executes the nodes in order, passing a [`FrameContext`] between them via two
//! ping-pong GPU targets. It special-cases nothing: builtin and (future)
//! external addons run through the identical [`FilterNode`] interface.
//!
//! # Architecture phases wired here
//!
//! - **Phase 1 (D002, D006):** [`graph`] — `RenderGraph` skeleton wraps the
//!   node list with graph-local slot ordering. `FilterNode` is kept internally;
//!   no public rename yet.
//! - **Phase 2 (D004, I002, I006, I015, I018):** [`plan`] — `ExecutionPlan` +
//!   `PlanEpoch`. `build()` compiles an immutable plan; `render()` executes it.
//! - **Phase 3 (D003, D005, I001–I019):** [`artifact`] + [`host_api`] —
//!   Semantic `RenderArtifact` ABI. `HostApi::publish_artifact()` is the only
//!   legal path for addon logic to submit render intent.

pub mod artifact;
pub mod broker;
pub mod context;
pub mod graph;
pub mod host;
pub mod host_api;
pub mod packing;
pub mod plan;
pub mod signals_group;
pub mod sink;
pub mod targets;

use std::collections::HashMap;
use std::path::Path;

use wgpu::*;

use crate::addon::manifest::Manifest;
use crate::addon::pipeline::PipelineConfig;
use crate::addon::registry::{AddonEntry, AddonRegistry};
use crate::addon::{AddonError, Result};
use crate::behavior::{BehaviorFactory, BehaviorRegistry};
use crate::engine::image::ImageBinding;

pub use context::{
    params_bind_group, params_layout, BuiltinAddon, FilterNode, FrameContext, NodeFactory,
    ResolvedConfig, SignalContext,
};
pub use signals_group::{signals_layout, SignalsBinding};
// Phase 1 (D002, D006) — RenderGraph skeleton.
pub use graph::RenderGraph;
// Phase 2 (D004, I006) — ExecutionPlan + PlanEpoch.
pub use plan::{ExecutionPlan, PlanEpoch};
// Phase 3 (D003, D005) — Semantic Artifact ABI.
pub use artifact::{
    ArtifactBudget, ArtifactValidationError, InstanceSchema, PrimitiveTopology, RenderArtifact,
    SemanticField, SemanticRow, SemanticRows, SemanticValue, TextMode, VisualContent,
};
pub use host_api::{HostApi, PublishError, StagedArtifact};
// Phase 4 (D003, I014, I020) — ResourceBroker.
pub use broker::{
    BrokerKey, BrokerMetrics, MaterializeError, MaterializedHandle, ResourceBroker,
};
pub use packing::{LayoutPlan, PackingProfile, PackingResult};
use host::HostUniform;
use sink::WindowSink;
use targets::RenderTarget;

use crate::signal::{SignalSchema, SignalSnapshot};

/// The source kind v1 ships. Sources are engine-shipped, not addons; the engine
/// owns the webcam texture and hands its view to the runtime.
pub const SOURCE_WEBCAM: &str = "webcam";
/// The sink kind v1 ships.
pub const SINK_WINDOW: &str = "window";

pub struct PipelineRuntime {
    registry: AddonRegistry,
    factories: HashMap<String, NodeFactory>,
    /// Manifests of the builtin addons, kept so a registry rescan (after an
    /// install/uninstall) can re-register them alongside the on-disk addons.
    builtin_manifests: Vec<Manifest>,
    /// Manifests of behavior addons registered in-code (registered for
    /// listing/validation; they have no filter factory — the behavior runtime
    /// constructs them). Re-registered on a rescan after install/uninstall.
    behavior_manifests: Vec<Manifest>,
    /// Behavior id → instance factory (the Phase 3 execution seam). The engine
    /// creates behavior instances by lookup here, never by a hardcoded match.
    behavior_registry: BehaviorRegistry,

    // Host-owned GPU resources, shared by every node.
    host: HostUniform,
    image: ImageBinding,
    format: TextureFormat,
    size: [f32; 2],

    // Ping-pong targets and their (cached) input bind groups.
    targets: [RenderTarget; 2],
    bg_targets: [BindGroup; 2],
    /// Bind group for the source texture (the first node's input). Set once the
    /// engine hands over the source view via [`set_source`](Self::set_source).
    bg_source: Option<BindGroup>,

    sink: WindowSink,

    // ---- Phase 1 (D002, D006): RenderGraph skeleton ----------------------------
    /// The live, instantiated pipeline wrapped in a `RenderGraph` for
    /// graph-local slot ordering. `FilterNode` is preserved internally (D006).
    graph: RenderGraph,

    // ---- Phase 2 (D004, I002, I006, I015, I018): ExecutionPlan -----------------
    /// Current monotonic epoch. Incremented on every successful `build()`.
    current_epoch: PlanEpoch,
    /// The compiled plan for the current epoch. `None` until the first
    /// successful `build()`. Replaced atomically on each rebuild (I018).
    current_plan: Option<ExecutionPlan>,

    // ---- Phase 4 (D003, I014, I016, I020): ResourceBroker ----------------------
    /// GPU buffer lifecycle manager. The Broker is the **only** entity that
    /// calls `device.create_buffer()` or `queue.write_buffer()` for dynamic
    /// render data (D003, I001). All addons go through RenderArtifact → Broker.
    broker: ResourceBroker,

    /// Number of successful pipeline (re)builds. Used by the spike to assert
    /// that signal-driven updates never trigger a rebuild.
    build_count: u64,
}

impl PipelineRuntime {
    pub fn new(device: &Device, format: TextureFormat, width: u32, height: u32) -> Self {
        let host = HostUniform::new(device);
        let image = ImageBinding::new(device);
        let targets = [
            RenderTarget::new(device, format, width, height),
            RenderTarget::new(device, format, width, height),
        ];
        let bg_targets = [
            image.bind_group(device, &targets[0].view),
            image.bind_group(device, &targets[1].view),
        ];
        let sink = WindowSink::new(device, &image.layout, format);

        Self {
            registry: AddonRegistry::new(),
            factories: HashMap::new(),
            builtin_manifests: Vec::new(),
            behavior_manifests: Vec::new(),
            behavior_registry: BehaviorRegistry::new(),
            host,
            image,
            format,
            size: [width as f32, height as f32],
            targets,
            bg_targets,
            bg_source: None,
            sink,
            // Phase 1 (D002, D006)
            graph: RenderGraph::new(),
            // Phase 2 (D004, I006)
            current_epoch: PlanEpoch::ZERO,
            current_plan: None,
            // Phase 4 (D003, I014, I016, I020): 64 MiB default budget.
            broker: ResourceBroker::new(64 * 1024 * 1024),
            build_count: 0,
        }
    }

    /// Register a builtin addon: its manifest goes into the registry (validated
    /// and compatibility-checked like any addon) and its factory into the
    /// instantiation table. After this, the addon is indistinguishable from an
    /// externally installed one as far as validation and execution are
    /// concerned.
    pub fn register_builtin<A: BuiltinAddon>(&mut self) -> Result<()> {
        let manifest = A::manifest();
        let id = manifest.id.clone();
        self.builtin_manifests.push(manifest.clone());
        self.registry.register_builtin(manifest)?;
        self.factories.insert(id, A::instantiate as NodeFactory);
        Ok(())
    }

    /// Register a behavior addon by manifest only — it appears in the registry
    /// (for UI listing + schema validation) but has **no factory**, so it is not
    /// executable (a reference/non-executable producer). Kept as the manifest-only
    /// half of the seam; [`register_behavior_with`](Self::register_behavior_with)
    /// is the executable path.
    #[allow(dead_code)] // retained API surface; executable behaviors use `_with`
    pub fn register_behavior(&mut self, manifest: Manifest) -> Result<()> {
        self.behavior_manifests.push(manifest.clone());
        self.registry.register_builtin(manifest)
    }

    /// Register an **executable** behavior addon: bind a [`BehaviorFactory`] to
    /// its id (the Phase 3 seam) and, unless a scanned on-disk package already
    /// provided the manifest, register that manifest for UI/validation. Call
    /// after [`scan_addons`](Self::scan_addons) so a compiled factory can attach
    /// to a package discovered on disk (the package is then authoritative for the
    /// UI param schema; the factory only supplies execution).
    pub fn register_behavior_with(
        &mut self,
        manifest: Manifest,
        factory: BehaviorFactory,
    ) -> Result<()> {
        self.behavior_registry.register(&manifest.id, factory);
        if !self.registry.contains(&manifest.id) {
            self.behavior_manifests.push(manifest.clone());
            self.registry.register_builtin(manifest)?;
        }
        Ok(())
    }

    /// The behavior factory registry — the engine resolves `pipeline.json`
    /// behavior entries to runnable instances through this (by lookup, not by
    /// name).
    pub fn behavior_registry(&self) -> &BehaviorRegistry {
        &self.behavior_registry
    }

    /// Scan an addons directory for on-disk addons, in addition to whatever is
    /// already registered (builtins). Missing directory → no-op. Used at
    /// startup to pick up installed addons.
    pub fn scan_addons(&mut self, root: &Path) -> Result<()> {
        self.registry.scan(root)
    }

    /// Rebuild the registry from scratch after an install/uninstall: re-register
    /// the builtins, then rescan the addons directory. The live node list is
    /// untouched (a separate `build` applies any resulting changes).
    pub fn rescan_addons(&mut self, root: &Path) -> Result<()> {
        self.registry.clear();
        for manifest in &self.builtin_manifests {
            self.registry.register_builtin(manifest.clone())?;
        }
        for manifest in &self.behavior_manifests {
            self.registry.register_builtin(manifest.clone())?;
        }
        self.registry.scan(root)
    }

    /// Whether the runtime has a factory able to instantiate `addon_id`. An
    /// installed addon without an implementation (e.g. a future external addon)
    /// can be listed but not run.
    pub fn has_implementation(&self, addon_id: &str) -> bool {
        self.factories.contains_key(addon_id)
    }

    /// Read-only access to the addon registry `build` validates against. The UI
    /// uses this to list installed addons and resolve display names / param
    /// schemas — it never mutates the registry or touches node internals.
    pub fn registry(&self) -> &AddonRegistry {
        &self.registry
    }

    /// The engine hands the runtime the source texture view (the webcam). The
    /// view is stable for the program's life, so its bind group is built once.
    pub fn set_source(&mut self, device: &Device, view: &TextureView) {
        self.bg_source = Some(self.image.bind_group(device, view));
    }

    /// Validate `config` against the registry, then instantiate every enabled
    /// node. Rejects unknown sources/sinks, unknown or incompatible addons, and
    /// invalid params *before* any rendering happens. On success the live
    /// pipeline replaces whatever was running.
    pub fn build(
        &mut self,
        device: &Device,
        config: &PipelineConfig,
        schema: &SignalSchema,
    ) -> Result<()> {
        config.validate_structure()?;

        if config.source.kind != SOURCE_WEBCAM {
            return Err(AddonError::UnsupportedSource(config.source.kind.clone()));
        }
        if config.sink.kind != SINK_WINDOW {
            return Err(AddonError::UnsupportedSink(config.sink.kind.clone()));
        }

        // Reject invalid / incompatible addons and bad params up front.
        let issues = config.validate_against(&self.registry);
        if !issues.is_empty() {
            let rendered = issues
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            return Err(AddonError::PipelineRejected(rendered));
        }

        // Instantiate each enabled node through its addon's own factory.
        // Phase 1 (D002, D006): push into RenderGraph instead of bare Vec.
        let mut new_graph = RenderGraph::new();
        for node in &config.pipeline {
            if !node.enabled {
                continue;
            }
            let entry = self
                .registry
                .get(&node.addon)
                .expect("validate_against guarantees the addon is installed");
            let resolved = ResolvedConfig::new(&entry.manifest.params, &node.config);
            let signals = SignalContext::new(schema, &entry.manifest.consume);

            let instance = match self.factories.get(&node.addon) {
                // A compiled-in addon (builtin) supplies its own factory.
                Some(factory) => factory(
                    device,
                    &self.host.layout,
                    &self.image.layout,
                    self.format,
                    &resolved,
                    &signals,
                ),
                // Otherwise fall back to the generic external-shader runner,
                // which loads the addon's declared WGSL off disk and wires its
                // declared `consume` signals into `@group(3)` (same as builtins).
                None => self.build_external(device, entry, &resolved, &signals)?,
            };
            new_graph.push(instance);
        }

        // Phase 2 (D004, I006, I015): advance epoch, compile immutable plan.
        // compile() is the only ExecutionPlan constructor — frame execution
        // cannot mutate this plan (I018).
        let new_epoch = self.current_epoch.next();
        let new_plan = ExecutionPlan::compile(new_epoch, &new_graph);

        self.graph = new_graph;
        self.current_epoch = new_epoch;
        self.current_plan = Some(new_plan);
        self.build_count += 1;
        Ok(())
    }

    /// Number of successful pipeline (re)builds since start. Constant while only
    /// signals change — the spike's core invariant.
    pub fn build_count(&self) -> u64 {
        self.build_count
    }

    /// Generic runner for an external addon (no compiled factory): load the
    /// addon's declared fragment shader off disk and pack its numeric schema
    /// params into the `@group(2)` uniform. An addon with no shader genuinely
    /// has nothing to run → [`AddonError::NoImplementation`].
    fn build_external(
        &self,
        device: &Device,
        entry: &AddonEntry,
        resolved: &ResolvedConfig,
        signals: &SignalContext,
    ) -> Result<Box<dyn FilterNode>> {
        let shader = entry
            .manifest
            .shaders
            .iter()
            .find(|s| s.stage == "fragment")
            .or_else(|| entry.manifest.shaders.first())
            .ok_or_else(|| AddonError::NoImplementation(entry.manifest.id.clone()))?;

        let path = entry.root.join(&shader.path);
        let src = std::fs::read_to_string(&path).map_err(|e| {
            AddonError::Package(format!("failed to read shader {:?}: {e}", shader.path))
        })?;

        // Numeric params packed in sorted-key order (BTreeMap iterates sorted),
        // matching the convention the addon's `@group(2)` struct must follow.
        let params: Vec<f32> = entry
            .manifest
            .params
            .keys()
            .map(|k| resolved.f32(k))
            .collect();

        Ok(crate::addons::external_shader_node(
            device,
            &self.host.layout,
            &self.image.layout,
            self.format,
            &entry.manifest.id,
            &src,
            &params,
            signals,
        ))
    }

    /// Recreate the ping-pong targets at a new surface size. The instantiated
    /// nodes are size-independent (they read resolution from the host uniform),
    /// so they survive a resize untouched.
    pub fn resize(&mut self, device: &Device, width: u32, height: u32) {
        self.size = [width as f32, height as f32];
        self.targets = [
            RenderTarget::new(device, self.format, width, height),
            RenderTarget::new(device, self.format, width, height),
        ];
        self.bg_targets = [
            self.image.bind_group(device, &self.targets[0].view),
            self.image.bind_group(device, &self.targets[1].view),
        ];
    }

    /// Execute the pipeline for one frame and present it to `surface_view`.
    ///
    /// Node `i` reads the source (i == 0) or the previous node's target, and
    /// writes target `i & 1`; the sink blits the final target. With an empty
    /// pipeline the source is blitted straight through.
    pub fn render(
        &mut self,
        device: &Device,
        queue: &Queue,
        surface_view: &TextureView,
        time: f32,
        signals: &SignalSnapshot,
    ) {
        // Phase 4 (I020): Prepare Phase begins here. The Broker is the ONLY
        // entity that may call create_buffer or write_buffer for dynamic data.
        // begin_frame() transitions Active → Idle for stale resources so the
        // Grace Window sweeper can reclaim them post-frame.
        self.broker.begin_frame();

        self.host.upload(queue, self.size, time);

        // Phase 2 (I002, I018): read the compiled plan. Execution cannot mutate
        // the graph topology — we only read node_count from the plan.
        let node_count = self
            .current_plan
            .as_ref()
            .map(|p| p.node_count)
            .unwrap_or(0);
        debug_assert_eq!(
            node_count,
            self.graph.len(),
            "ExecutionPlan node_count must match live graph len (I015)"
        );

        // Signal-binding pass: each node folds the latest snapshot into its own
        // per-frame uniforms via `queue.write_buffer`. No rebuild, no new bind
        // groups, no pipeline recompilation — just bytes uploaded to existing
        // buffers. Nodes that consume nothing do nothing here.
        // Phase 1 (D002): iterate over RenderGraph nodes.
        for gn in self.graph.nodes_mut() {
            gn.node.prepare(queue, signals);
        }

        let source_bg = self
            .bg_source
            .as_ref()
            .expect("set_source must be called before render");

        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("pipeline_encoder"),
        });

        let n = node_count;
        for (i, gn) in self.graph.nodes().iter().enumerate() {
            let input_bg = if i == 0 {
                source_bg
            } else {
                &self.bg_targets[(i - 1) & 1]
            };
            let mut ctx = FrameContext {
                encoder: &mut encoder,
                host_bg: &self.host.bind_group,
                input_bg,
                output: &self.targets[i & 1].view,
            };
            gn.node.process(&mut ctx);
        }

        let final_bg = if n == 0 {
            source_bg
        } else {
            &self.bg_targets[(n - 1) & 1]
        };
        self.sink.blit(&mut encoder, final_bg, surface_view);

        queue.submit(Some(encoder.finish()));

        // Phase 4 (I020): Post-frame sweep. Evict idle resources whose Grace
        // Window has expired. Called AFTER submit so GPU work is already queued;
        // dropping the buffer here is safe because the GPU references a handle,
        // not the Rust object. Buffers in Active state are never evicted.
        self.broker.sweep();
    }

    /// Read-only access to the Broker for diagnostics / debug visualizer.
    pub fn broker(&self) -> &ResourceBroker {
        &self.broker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::pipeline::{SinkConfig, SourceConfig};
    use crate::addons::{CrtAddon, DotRendererAddon};

    fn default_source() -> SourceConfig {
        SourceConfig {
            kind: SOURCE_WEBCAM.into(),
            config: serde_json::Value::Object(Default::default()),
        }
    }
    fn default_sink() -> SinkConfig {
        SinkConfig {
            kind: SINK_WINDOW.into(),
            config: serde_json::Value::Object(Default::default()),
        }
    }

    /// Builtin addons register and validate through the same metadata path an
    /// external addon would (no GPU needed for this part).
    #[test]
    fn builtin_addons_register_and_validate() {
        let mut registry = AddonRegistry::new();
        registry
            .register_builtin(DotRendererAddon::manifest())
            .unwrap();
        registry.register_builtin(CrtAddon::manifest()).unwrap();

        assert!(registry.contains("dot-renderer"));
        assert!(registry.contains("crt"));
        assert!(registry.get("crt").unwrap().builtin);

        // The README pipeline: webcam → dot-renderer → crt → window.
        let mut config = PipelineConfig::new(default_source(), default_sink());
        config.add_node("dot-renderer", None);
        config.add_node("crt", None);

        let issues = config.validate_against(&registry);
        assert!(issues.is_empty(), "unexpected issues: {issues:?}");
    }

    #[test]
    fn unknown_addon_is_rejected_by_validation() {
        let mut registry = AddonRegistry::new();
        registry
            .register_builtin(DotRendererAddon::manifest())
            .unwrap();

        let mut config = PipelineConfig::new(default_source(), default_sink());
        config.add_node("does-not-exist", None);

        let issues = config.validate_against(&registry);
        assert_eq!(issues.len(), 1);
    }

    /// A real builtin manifest must pass its own structural validation,
    /// including the "param defaults satisfy their own spec" rule.
    #[test]
    fn builtin_manifests_are_self_consistent() {
        DotRendererAddon::manifest().validate().unwrap();
        CrtAddon::manifest().validate().unwrap();
    }
}
