//! The runtime↔addon execution contract.
//!
//! A filter addon is anything that implements [`FilterNode`]: it receives a
//! [`FrameContext`], records GPU work that reads the frame input and writes the
//! frame output, and returns. It does not know its neighbours, its position in
//! the pipeline, or the source/sink on either end.
//!
//! Builtin addons additionally implement [`BuiltinAddon`] so the runtime can
//! register their manifest and a factory through the same path an external
//! addon would use.

use std::collections::BTreeMap;

use wgpu::*;

use crate::addon::manifest::Manifest;
use crate::addon::schema::{ParamMap, ParamSpec, ParamValue};
use crate::signal::SignalSnapshot;

/// Everything a node needs to record one frame's worth of work.
///
/// The runtime drives the ping-pong: `input_bg` binds the previous node's
/// output (or the source) at `@group(1)`, and `output` is the texture this node
/// renders into. Resources stay on the GPU — no CPU image buffers cross here.
pub struct FrameContext<'a> {
    pub encoder: &'a mut CommandEncoder,
    /// `@group(0)` — host context (resolution + time), shared by every node.
    pub host_bg: &'a BindGroup,
    /// `@group(1)` — the frame input for this node.
    pub input_bg: &'a BindGroup,
    /// The texture view this node must render its result into.
    pub output: &'a TextureView,
}

/// A live, instantiated filter node. The runtime holds these as
/// `Box<dyn FilterNode>` and executes them in order, identically — there is
/// no per-addon branching anywhere in the executor.
///
/// Each frame the runtime calls [`prepare`](FilterNode::prepare) once (to fold
/// the latest [`SignalSnapshot`] into the node's per-frame uniforms via
/// `queue.write_buffer`), then [`process`](FilterNode::process) (to record the
/// render pass). Nodes that consume no signals keep the default no-op `prepare`.
pub trait FilterNode {
    /// Refresh per-frame GPU state from the latest signals. Default no-op.
    /// This must only *update* existing resources (e.g. `write_buffer`); it
    /// must never recreate bind groups, pipelines, or rebuild the runtime.
    fn prepare(&mut self, _queue: &Queue, _signals: &SignalSnapshot) {}

    /// Record this node's render pass.
    fn process(&self, ctx: &mut FrameContext);
}

/// An addon that ships inside the engine binary. It exposes the same two things
/// an external addon would: a manifest (identity + params + compatibility) and
/// a way to instantiate a node from resolved config.
pub trait BuiltinAddon {
    /// The addon's manifest — registered into the [`AddonRegistry`] verbatim.
    fn manifest() -> Manifest;

    /// Build a live node. `host_layout` and `image_layout` are the runtime's
    /// `@group(0)`/`@group(1)` layouts; `format` is the render-target format;
    /// `config` resolves manifest defaults against the node's pipeline config.
    fn instantiate(
        device: &Device,
        host_layout: &BindGroupLayout,
        image_layout: &BindGroupLayout,
        format: TextureFormat,
        config: &ResolvedConfig,
    ) -> Box<dyn FilterNode>;
}

/// Function-pointer form of [`BuiltinAddon::instantiate`], stored per addon id.
pub type NodeFactory = fn(
    &Device,
    &BindGroupLayout,
    &BindGroupLayout,
    TextureFormat,
    &ResolvedConfig,
) -> Box<dyn FilterNode>;

/// A node's configuration with manifest defaults filled in. Validation has
/// already run by the time an addon sees this, so the typed accessors fall back
/// to the declared default for any key that is unset or (defensively) the wrong
/// type — an addon never has to handle a missing param.
pub struct ResolvedConfig<'a> {
    specs: &'a BTreeMap<String, ParamSpec>,
    values: &'a ParamMap,
}

impl<'a> ResolvedConfig<'a> {
    pub fn new(specs: &'a BTreeMap<String, ParamSpec>, values: &'a ParamMap) -> Self {
        Self { specs, values }
    }

    fn value(&self, key: &str) -> Option<ParamValue> {
        self.values
            .get(key)
            .cloned()
            .or_else(|| self.specs.get(key).map(ParamSpec::default_value))
    }

    pub fn f32(&self, key: &str) -> f32 {
        match self.value(key) {
            Some(ParamValue::F32(x)) => x as f32,
            Some(ParamValue::I32(i)) => i as f32,
            _ => 0.0,
        }
    }

    #[allow(dead_code)] // part of the accessor trio; for future int-param addons
    pub fn i32(&self, key: &str) -> i32 {
        match self.value(key) {
            Some(ParamValue::I32(i)) => i as i32,
            Some(ParamValue::F32(x)) => x as i32,
            _ => 0,
        }
    }

    pub fn bool(&self, key: &str) -> bool {
        matches!(self.value(key), Some(ParamValue::Bool(true)))
    }
}

/// Build the `@group(2)` layout every addon uses for its params uniform: a
/// single fragment-visible uniform buffer at binding 0. Shared so addons don't
/// each re-spell the same descriptor.
pub fn params_layout(device: &Device, label: &str) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[BindGroupLayoutEntry {
            binding: 0,
            visibility: ShaderStages::FRAGMENT,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    })
}

/// Build the bind group for a params uniform buffer against [`params_layout`].
pub fn params_bind_group(device: &Device, layout: &BindGroupLayout, buffer: &Buffer) -> BindGroup {
    device.create_bind_group(&BindGroupDescriptor {
        label: Some("addon_params_bind_group"),
        layout,
        entries: &[BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    })
}
