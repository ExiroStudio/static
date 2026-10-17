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
pub mod identity;
pub mod native;
pub mod packing;
pub mod plan;
pub mod reflection;
pub mod signals_group;
pub mod sink;
pub mod targets;

pub use native::NativePipelineBridge;
pub use identity::{NodeHandle, ArtifactHandle, SchemaHandle, hash_str, CURRENT_FRAME, relative_time};

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

    /// Last frame timestamp for delta calculations.
    last_time: Option<f32>,
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
            last_time: None,
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

    /// Scan multiple directories for on-disk addons.
    pub fn scan_addons_all<P: AsRef<Path>>(
        &mut self,
        roots: impl IntoIterator<Item = P>,
    ) -> Result<()> {
        self.registry.scan_all(roots)
    }

    /// Rebuild the registry from scratch after an install/uninstall: re-register
    /// the builtins, then rescan the addons directory. The live node list is
    /// untouched (a separate `build` applies any resulting changes).
    pub fn rescan_addons(&mut self, root: &Path) -> Result<()> {
        self.rescan_addons_all([root])
    }

    /// Rebuild the registry from scratch after an install/uninstall: re-register
    /// the builtins, then rescan multiple addon directories.
    pub fn rescan_addons_all<P: AsRef<Path>>(
        &mut self,
        roots: impl IntoIterator<Item = P>,
    ) -> Result<()> {
        self.registry.clear();
        for manifest in &self.builtin_manifests {
            self.registry.register_builtin(manifest.clone())?;
        }
        for manifest in &self.behavior_manifests {
            self.registry.register_builtin(manifest.clone())?;
        }
        self.registry.scan_all(roots)
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
        println!("[PIPELINE] Pipeline build start");
        config.validate_structure()?;

        if config.source.kind != SOURCE_WEBCAM {
            println!("[ERROR] Unsupported source: {}", config.source.kind);
            return Err(AddonError::UnsupportedSource(config.source.kind.clone()));
        }
        if config.sink.kind != SINK_WINDOW {
            println!("[ERROR] Unsupported sink: {}", config.sink.kind);
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
            println!("[ERROR] Pipeline rejected with issues: {}", rendered);
            return Err(AddonError::PipelineRejected(rendered));
        }

        // Instantiate each enabled node through its addon's own factory.
        // Phase 1 (D002, D006): push into RenderGraph instead of bare Vec.
        let mut new_graph = RenderGraph::new();
        println!("[PIPELINE] Pipeline node count: {}", config.pipeline.len());
        for (node_idx, node) in config.pipeline.iter().enumerate() {
            if !node.enabled {
                println!("[PIPELINE] Skipping disabled render node '{}'", node.instance_id);
                continue;
            }
            let node_handle = NodeHandle::new(node_idx as u32);
            println!(
                "[PIPELINE] Render node mapping: render node '{}' (addon: '{}') assigned NodeHandle({})",
                node.instance_id, node.addon, node_handle.raw()
            );
            let entry = self
                .registry
                .get(&node.addon)
                .expect("validate_against guarantees the addon is installed");
            let resolved = ResolvedConfig::new(&entry.manifest.params, &node.config);
            let signals = SignalContext::new(schema, &entry.manifest.consume);

            let instance: Box<dyn FilterNode> = match self.factories.get(&node.addon) {
                // A compiled-in addon (builtin) supplies its own factory.
                Some(factory) => factory(
                    device,
                    &self.host.layout,
                    &self.image.layout,
                    self.format,
                    &resolved,
                    &signals,
                ),
                // Otherwise check if it is a native runner
                None => {
                    if entry.manifest.runner.as_deref() == Some("native") {
                        let entry_path = entry.root.join(
                            entry.manifest.entry.as_ref()
                                .ok_or_else(|| AddonError::Package("native addon missing entry field in manifest".into()))?
                        );
                        
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

                        let params: Vec<f32> = entry
                            .manifest
                            .params
                            .keys()
                            .map(|k| resolved.f32(k))
                            .collect();

                        let schema_decl = entry.manifest.pipeline.as_ref()
                            .and_then(|p| p.instance_schema.as_ref())
                            .ok_or_else(|| AddonError::Package("instanced pipeline requires instance_schema".into()))?;

                        let semantic_fields: Vec<SemanticField> = schema_decl.fields.iter().map(|f| match f.kind.as_str() {
                            "vec2" => {
                                if f.name == "position" || f.name == "pos" {
                                    SemanticField::Position2
                                } else {
                                    SemanticField::Position2
                                }
                            }
                            "vec3" => SemanticField::Position3,
                            "vec4" => {
                                if f.name == "color" || f.name == "colour" {
                                    SemanticField::ColorRgba
                                } else if f.name == "uv" || f.name == "tex_coords" {
                                    SemanticField::UvQuad
                                } else {
                                    SemanticField::ColorRgba
                                }
                            }
                            "float" => SemanticField::CustomFloat(f.name.clone()),
                            _ => SemanticField::CustomFloat(f.name.clone()),
                        }).collect();

                        let layout_plan = LayoutPlan::compile(&semantic_fields, PackingProfile::VertexInstanced);
                        
                        let instance_schema = InstanceSchema {
                            schema_id: schema_decl.schema_id,
                            fields: semantic_fields,
                        };

                        let vertex_entry = entry.manifest.shaders.iter()
                            .find(|s| s.stage == "vertex")
                            .map(|s| s.entry.as_str())
                            .unwrap_or("vs_main");

                        let fragment_entry = entry.manifest.shaders.iter()
                            .find(|s| s.stage == "fragment")
                            .map(|s| s.entry.as_str())
                            .unwrap_or("fs_main");

                         Box::new(NativePipelineBridge::new(
                            node_handle,
                            device,
                            &self.host.layout,
                            &self.image.layout,
                            self.format,
                            entry_path,
                            &entry.manifest.id,
                            &src,
                            &params,
                            &signals,
                            &layout_plan,
                            instance_schema,
                            node.config.clone(),
                            vertex_entry,
                            fragment_entry,
                        ))
                    } else {
                        self.build_external(device, entry, &resolved, &signals)?
                    }
                }
            };
            println!("[PIPELINE] Pushing node to RenderGraph: NodeHandle({}), instance_id: '{}'", node_handle.raw(), node.instance_id);
            new_graph.push(node_handle, node.instance_id.clone(), instance);
        }

        // Phase 2 (D004, I006, I015): advance epoch, compile immutable plan.
        // compile() is the only ExecutionPlan constructor — frame execution
        // cannot mutate this plan (I018).
        let new_epoch = self.current_epoch.next();
        let new_plan = ExecutionPlan::compile(new_epoch, &new_graph);
        println!(
            "[PIPELINE] ExecutionPlan created: epoch = {}, node count = {}, hash = {}",
            new_epoch.raw(), new_plan.node_count, new_plan.plan_hash
        );

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
        use crate::addon::manifest::PipelineRenderer;
        
        let renderer = entry
            .manifest
            .pipeline
            .as_ref()
            .map(|p| p.renderer)
            .unwrap_or(PipelineRenderer::Fullscreen);

        match renderer {
            PipelineRenderer::Fullscreen => {
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
            PipelineRenderer::Instanced => {
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

                let params: Vec<f32> = entry
                    .manifest
                    .params
                    .keys()
                    .map(|k| resolved.f32(k))
                    .collect();

                let schema = entry.manifest.pipeline.as_ref()
                    .and_then(|p| p.instance_schema.as_ref())
                    .ok_or_else(|| AddonError::Package("instanced pipeline requires instance_schema".into()))?;

                use crate::runtime::artifact::SemanticField;
                use crate::runtime::packing::{LayoutPlan, PackingProfile};

                let semantic_fields: Vec<SemanticField> = schema.fields.iter().map(|f| match f.kind.as_str() {
                    "vec2" => SemanticField::Position2,
                    "vec3" => SemanticField::Position3,
                    "vec4" => SemanticField::ColorRgba,
                    "float" => SemanticField::CustomFloat(f.name.clone()),
                    _ => SemanticField::CustomFloat(f.name.clone()),
                }).collect();

                let layout_plan = LayoutPlan::compile(&semantic_fields, PackingProfile::VertexInstanced);

                let vertex_entry = entry.manifest.shaders.iter()
                    .find(|s| s.stage == "vertex")
                    .map(|s| s.entry.as_str())
                    .unwrap_or("vs_main");

                let fragment_entry = entry.manifest.shaders.iter()
                    .find(|s| s.stage == "fragment")
                    .map(|s| s.entry.as_str())
                    .unwrap_or("fs_main");

                Ok(crate::addons::instanced_external_node(
                    device,
                    &self.host.layout,
                    &self.image.layout,
                    self.format,
                    &entry.manifest.id,
                    &src,
                    &params,
                    signals,
                    &layout_plan,
                    vertex_entry,
                    fragment_entry,
                ))
            }
        }
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
        staged_artifacts: Vec<(crate::runtime::NodeHandle, crate::runtime::ArtifactHandle, crate::runtime::artifact::RenderArtifact)>,
    ) {
        if self.current_plan.is_none() {
            crate::trace_log!("WARNING", "render called but current_plan is None; skipping render");
            return; // no valid build
        }

        crate::trace_log!("RUNTIME", "EXECUTION PLAN");
        crate::trace_log!("RUNTIME", "  active graph nodes count: {}", self.graph.len());
        for gn in self.graph.nodes() {
            crate::trace_log!(
                "RUNTIME",
                "  node: slot = {}, instance_id = {}, NodeHandle({})",
                gn.slot,
                gn.instance_id,
                gn.node_handle.raw()
            );
        }

        let dt = if let Some(last) = self.last_time {
            time - last
        } else {
            0.0
        };
        self.last_time = Some(time);

        self.host.upload(queue, self.size, time);

        // Stage artifacts through the HostApi
        let mut host_api = crate::runtime::HostApi::new(
            self.current_epoch,
            crate::runtime::ArtifactBudget::default(),
        );
        host_api.set_timing(time, dt);

        for (node_handle, art_handle, artifact) in staged_artifacts {
            if let Err(e) = host_api.publish_artifact_v2(node_handle, art_handle, artifact, self.current_epoch) {
                crate::trace_log!("WARNING", "artifact validation failed: {:?}", e);
            }
        }
        
        // Update loop for filter nodes (enabling NativePipelineBridge CPU work)
        for gn in self.graph.nodes_mut() {
            gn.node.update(&mut host_api);
        }
        
        let valid_artifacts = host_api.drain_staged();
        for staged in valid_artifacts {
            let key = crate::runtime::BrokerKey::new(self.current_epoch, staged.node_handle, staged.artifact_handle);
            if let Err(e) = self.broker.materialize(device, queue, key.clone(), &staged.artifact) {
                crate::trace_log!("WARNING", "materialization failed: {:?}", e);
            }
            if let Some(res) = self.broker.get(&key) {
                crate::trace_log!("RUNTIME", "Handle Created");
                crate::trace_log!("RUNTIME", "  rows: {}", res.last_row_count);
                crate::trace_log!("RUNTIME", "  stride: {}", res.layout.as_ref().map(|l| l.stride()).unwrap_or(0));
                crate::trace_log!("RUNTIME", "  buffer exists: true");
            }
        }

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
        crate::trace_log!("RUNTIME", "Preparing node signals");
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
        crate::trace_log!("RUNTIME", "Processing graph nodes: node_count = {}", n);
        for (i, gn) in self.graph.nodes().iter().enumerate() {
            crate::trace_log!(
                "RUNTIME",
                "Processing graph node: index = {}, NodeHandle = NodeHandle({}), instance_id = '{}'",
                i, gn.node_handle.raw(), gn.instance_id
            );
            let input_bg = if i == 0 {
                source_bg
            } else {
                &self.bg_targets[(i - 1) & 1]
            };

            // Look up materialized resources from broker
            let key = crate::runtime::BrokerKey::new(
                self.current_epoch,
                gn.node_handle,
                crate::runtime::ArtifactHandle::new(crate::runtime::hash_str("instances")),
            );
            let resources = self.broker.get(&key).map(|res| crate::runtime::context::MaterializedResources {
                instance_buffer: Some(&res.buffer),
                row_count: res.last_row_count as u32,
            });

            crate::trace_log!("RUNTIME", "Attach Resources");

            let mut ctx = FrameContext {
                encoder: &mut encoder,
                host_bg: &self.host.bind_group,
                input_bg,
                output: &self.targets[i & 1].view,
                resources,
            };

            crate::trace_log!("FRAME", "Frame context setup:");
            crate::trace_log!("FRAME", "  resources attached: {}", ctx.resources.is_some());
            if let Some(res) = &ctx.resources {
                crate::trace_log!("FRAME", "  instance buffer exists: {}", res.instance_buffer.is_some());
                crate::trace_log!("FRAME", "  row count: {}", res.row_count);
            } else {
                crate::trace_log!("FRAME", "  instance buffer exists: false");
                crate::trace_log!("FRAME", "  row count: 0");
            }

            crate::trace_log!(
                "RENDER_GRAPH",
                "RENDER GRAPH PASS START: node_index = {}, NodeHandle = NodeHandle({}), instance_id = '{}'",
                i, gn.node_handle.raw(), gn.instance_id
            );
            gn.node.process(&mut ctx);
            crate::trace_log!(
                "RENDER_GRAPH",
                "RENDER GRAPH PASS END: node_index = {}, NodeHandle = NodeHandle({})",
                i, gn.node_handle.raw()
            );
        }

        let final_bg = if n == 0 {
            source_bg
        } else {
            &self.bg_targets[(n - 1) & 1]
        };

        crate::trace_log!("PRESENT", "Executing final sink blit pass: node index = {}", n);
        self.sink.blit(&mut encoder, final_bg, surface_view);

        crate::trace_log!("QUEUE", "Submitting command encoder to queue");
        queue.submit(Some(encoder.finish()));

        crate::trace_log!("PRESENT", "Done");

        // Phase 4 (I020): Post-frame sweep. Evict idle resources whose Grace
        // Window has expired. Called AFTER submit so GPU work is already queued;
        // dropping the buffer here is safe because the GPU references a handle,
        // not the Rust object. Buffers in Active state are never evicted.
        println!("[RUNTIME] Initiating post-frame sweep on ResourceBroker");
        self.broker.sweep();
    }

    /// Read-only access to the Broker for diagnostics / debug visualizer.
    pub fn broker(&self) -> &ResourceBroker {
        &self.broker
    }

    pub fn current_epoch(&self) -> PlanEpoch {
        self.current_epoch
    }

    pub fn current_plan_hash(&self) -> Option<u64> {
        self.current_plan.as_ref().map(|p| p.plan_hash)
    }

    pub fn graph_len(&self) -> usize {
        self.graph.len()
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
