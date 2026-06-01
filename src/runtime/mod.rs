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
//! external addons run through the identical [`FilterNode`] interface. There
//! is no graph compilation, no dependency resolution, no scheduler — just
//! sequential execution.

pub mod context;
pub mod host;
pub mod sink;
pub mod targets;

use std::collections::HashMap;
use std::path::Path;

use wgpu::*;

use crate::addon::manifest::Manifest;
use crate::addon::pipeline::PipelineConfig;
use crate::addon::registry::{AddonEntry, AddonRegistry};
use crate::addon::{AddonError, Result};
use crate::engine::image::ImageBinding;

pub use context::{
    params_bind_group, params_layout, BuiltinAddon, FilterNode, FrameContext, NodeFactory,
    ResolvedConfig,
};
use host::HostUniform;
use sink::WindowSink;
use targets::RenderTarget;

use crate::signal::SignalSnapshot;

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

    /// The live, instantiated pipeline. Empty until [`build`](Self::build).
    nodes: Vec<Box<dyn FilterNode>>,

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
            host,
            image,
            format,
            size: [width as f32, height as f32],
            targets,
            bg_targets,
            bg_source: None,
            sink,
            nodes: Vec::new(),
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
    pub fn build(&mut self, device: &Device, config: &PipelineConfig) -> Result<()> {
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
        let mut nodes: Vec<Box<dyn FilterNode>> = Vec::new();
        for node in &config.pipeline {
            if !node.enabled {
                continue;
            }
            let entry = self
                .registry
                .get(&node.addon)
                .expect("validate_against guarantees the addon is installed");
            let resolved = ResolvedConfig::new(&entry.manifest.params, &node.config);

            let instance = match self.factories.get(&node.addon) {
                // A compiled-in addon (builtin) supplies its own factory.
                Some(factory) => factory(
                    device,
                    &self.host.layout,
                    &self.image.layout,
                    self.format,
                    &resolved,
                ),
                // Otherwise fall back to the generic external-shader runner,
                // which loads the addon's declared WGSL off disk.
                None => self.build_external(device, entry, &resolved)?,
            };
            nodes.push(instance);
        }

        self.nodes = nodes;
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
        self.host.upload(queue, self.size, time);

        // Signal-binding pass: each node folds the latest snapshot into its own
        // per-frame uniforms via `queue.write_buffer`. No rebuild, no new bind
        // groups, no pipeline recompilation — just bytes uploaded to existing
        // buffers. Nodes that consume nothing do nothing here.
        for node in self.nodes.iter_mut() {
            node.prepare(queue, signals);
        }

        let source_bg = self
            .bg_source
            .as_ref()
            .expect("set_source must be called before render");

        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("pipeline_encoder"),
        });

        let n = self.nodes.len();
        for (i, node) in self.nodes.iter().enumerate() {
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
            node.process(&mut ctx);
        }

        let final_bg = if n == 0 {
            source_bg
        } else {
            &self.bg_targets[(n - 1) & 1]
        };
        self.sink.blit(&mut encoder, final_bg, surface_view);

        queue.submit(Some(encoder.finish()));
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
