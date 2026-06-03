//! Builtin addons — addons that ship inside the engine binary.
//!
//! Each lives in its own file, declares its [`Manifest`] in code, and builds a
//! [`FilterNode`] from resolved config. They are registered through
//! [`PipelineRuntime::register_builtin`](crate::runtime::PipelineRuntime::register_builtin)
//! and from then on are indistinguishable from external addons — the runtime
//! never names them.
//!
//! [`Manifest`]: crate::addon::manifest::Manifest
//! [`FilterNode`]: crate::runtime::FilterNode

mod crt;
mod dot_renderer;

pub use crt::CrtAddon;
pub use dot_renderer::DotRendererAddon;

use std::collections::BTreeMap;

use wgpu::util::DeviceExt;
use wgpu::*;

use crate::addon::manifest::{AddonKind, Manifest, CURRENT_MANIFEST_VERSION};
use crate::addon::schema::{ParamSpec, UiHints};
use crate::effects::{fullscreen_pipeline, make_module};
use crate::runtime::{
    params_bind_group, params_layout, signals_layout, FilterNode, FrameContext, SignalContext,
    SignalsBinding,
};
use crate::signal::SignalSnapshot;

/// Record one fullscreen pass: bind `[host, input, params]` and draw the
/// fullscreen triangle into `output`. Shared by every node shape so the render
/// recording lives in one place.
pub(super) fn record_fullscreen_pass(
    ctx: &mut FrameContext,
    pipeline: &RenderPipeline,
    params_bg: &BindGroup,
    signals_bg: Option<&BindGroup>,
    label: &str,
) {
    let mut pass = ctx.encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: ctx.output,
            resolve_target: None,
            ops: Operations {
                load: LoadOp::Clear(Color::BLACK),
                store: StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        occlusion_query_set: None,
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, ctx.host_bg, &[]); // host context
    pass.set_bind_group(1, ctx.input_bg, &[]); // frame input
    pass.set_bind_group(2, params_bg, &[]); // addon params
    if let Some(bg) = signals_bg {
        pass.set_bind_group(3, bg, &[]); // dynamic bindings (signals)
    }
    pass.draw(0..3, 0..1);
}

/// The GPU pieces of a single fullscreen shader pass: the pipeline plus the
/// `@group(2)` params buffer and its bind group. Returned to addons that need
/// to keep the buffer to update it per frame (signal consumers).
pub(super) struct ShaderPass {
    pub pipeline: RenderPipeline,
    pub params_bg: BindGroup,
    pub params_buf: Buffer,
}

/// The shared shape of a builtin addon node: one fullscreen pass with a params
/// uniform at `@group(2)`. DotRenderer and CRT differ only in their shader and
/// the bytes of that uniform, so the GPU plumbing lives here once.
struct ShaderNode {
    label: String,
    pipeline: RenderPipeline,
    params_bg: BindGroup,
    // Kept alive for as long as the bind group references it.
    _params_buf: Buffer,
}

impl FilterNode for ShaderNode {
    fn process(&self, ctx: &mut FrameContext) {
        record_fullscreen_pass(ctx, &self.pipeline, &self.params_bg, None, self.label.as_str());
    }
}

/// Build a single-pass shader node from a typed params struct (used by the
/// builtins, whose layout is known at compile time).
fn build_shader_node<P: bytemuck::Pod>(
    device: &Device,
    host_layout: &BindGroupLayout,
    image_layout: &BindGroupLayout,
    format: TextureFormat,
    label: &'static str,
    shader_src: &str,
    params: P,
) -> Box<dyn FilterNode> {
    build_shader_node_bytes(
        device,
        host_layout,
        image_layout,
        format,
        label,
        shader_src,
        bytemuck::bytes_of(&params),
    )
}

/// Build the GPU pieces of a fullscreen shader pass: upload `params_bytes` to a
/// `@group(2)` uniform, compose `shader_src` with the shared prelude, and build
/// the pipeline bound as `[host, image, params]`. The params buffer is returned
/// so signal-consuming addons can update it each frame.
pub(super) fn build_shader_pass(
    device: &Device,
    host_layout: &BindGroupLayout,
    image_layout: &BindGroupLayout,
    format: TextureFormat,
    label: &str,
    shader_src: &str,
    params_bytes: &[u8],
    extra_layouts: &[&BindGroupLayout],
) -> ShaderPass {
    let params_buf = device.create_buffer_init(&util::BufferInitDescriptor {
        label: Some(label),
        contents: params_bytes,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
    });
    let plyt = params_layout(device, label);
    let params_bg = params_bind_group(device, &plyt, &params_buf);

    // Bind group layouts: [host, image, params, <extra (e.g. group3)>].
    let mut layouts: Vec<&BindGroupLayout> = vec![host_layout, image_layout, &plyt];
    layouts.extend_from_slice(extra_layouts);

    let module = make_module(device, label, shader_src);
    let pipeline = fullscreen_pipeline(device, label, &module, &layouts, format);

    ShaderPass {
        pipeline,
        params_bg,
        params_buf,
    }
}

/// Core node builder for nodes with no per-frame updates: wraps a
/// [`build_shader_pass`] result in a plain [`ShaderNode`].
fn build_shader_node_bytes(
    device: &Device,
    host_layout: &BindGroupLayout,
    image_layout: &BindGroupLayout,
    format: TextureFormat,
    label: &str,
    shader_src: &str,
    params_bytes: &[u8],
) -> Box<dyn FilterNode> {
    let pass = build_shader_pass(
        device,
        host_layout,
        image_layout,
        format,
        label,
        shader_src,
        params_bytes,
        &[],
    );
    Box::new(ShaderNode {
        label: label.to_string(),
        pipeline: pass.pipeline,
        params_bg: pass.params_bg,
        _params_buf: pass.params_buf,
    })
}

/// An external addon's node: one fullscreen pass plus, *only if the addon
/// declares `consume`*, a per-frame `@group(3)` signals uniform it refreshes
/// each frame. Mirrors the builtin CRT shape so external and builtin signal
/// consumers run through the identical [`FilterNode`] path.
struct ExternalShaderNode {
    label: String,
    pipeline: RenderPipeline,
    params_bg: BindGroup,
    // Kept alive for as long as the bind group references it.
    _params_buf: Buffer,
    /// `Some` iff the addon consumes ≥1 signal — then `@group(3)` exists and is
    /// updated every frame. `None` consumers keep the no-op default `prepare`
    /// and a pipeline layout byte-identical to a non-consuming addon.
    signals: Option<SignalsBinding>,
}

impl FilterNode for ExternalShaderNode {
    fn prepare(&mut self, queue: &Queue, signals: &SignalSnapshot) {
        // Pack the consumed signals into @group(3). No rebuild, no new resources.
        if let Some(binding) = self.signals.as_mut() {
            binding.update(queue, signals);
        }
    }

    fn process(&self, ctx: &mut FrameContext) {
        record_fullscreen_pass(
            ctx,
            &self.pipeline,
            &self.params_bg,
            self.signals.as_ref().map(SignalsBinding::bind_group),
            &self.label,
        );
    }
}

/// Build a node for an **external** addon from a shader loaded off disk.
///
/// This is the generic runner the runtime falls back to when a pipeline addon
/// has no compiled-in factory. It composes the addon's WGSL with the shared
/// prelude and packs its numeric schema parameters into the `@group(2)` uniform
/// as a tightly-packed `f32` array, padded to 16-byte alignment. The addon's
/// shader must declare a matching `@group(2)` struct (its parameters as `f32`
/// fields, in sorted-key order). Non-numeric params contribute `0.0`.
///
/// If the addon's manifest declares `consume = [...]`, the runner also builds
/// the `@group(3)` signals uniform — exactly as the builtin CRT does — and the
/// node refreshes it every frame via `prepare`. The shader reads each consumed
/// signal as a `vec4<f32>` slot in manifest `consume` order. Optional signals
/// that no behavior publishes resolve to a zero slot (the fallback). An addon
/// that consumes nothing never gets a `@group(3)`, so its pipeline layout is
/// unchanged. No path here rebuilds or recompiles on a signal change.
pub fn external_shader_node(
    device: &Device,
    host_layout: &BindGroupLayout,
    image_layout: &BindGroupLayout,
    format: TextureFormat,
    label: &str,
    shader_src: &str,
    params: &[f32],
    signals: &SignalContext,
) -> Box<dyn FilterNode> {
    // Pad to a multiple of 4 floats (16 bytes) so the uniform satisfies WGSL's
    // alignment rules; never zero-length.
    let mut floats = params.to_vec();
    while floats.is_empty() || floats.len() % 4 != 0 {
        floats.push(0.0);
    }

    // @group(3) only when the addon declares consumed signals. `None` keeps the
    // layout identical to a non-consuming addon (no extra bind group layout).
    let slayout = signals_layout(device);
    let binding = SignalsBinding::new(device, &slayout, signals);
    let extra: &[&BindGroupLayout] = if binding.is_some() { &[&slayout] } else { &[] };

    let pass = build_shader_pass(
        device,
        host_layout,
        image_layout,
        format,
        label,
        shader_src,
        bytemuck::cast_slice(&floats),
        extra,
    );
    Box::new(ExternalShaderNode {
        label: label.to_string(),
        pipeline: pass.pipeline,
        params_bg: pass.params_bg,
        _params_buf: pass.params_buf,
        signals: binding,
    })
}

// ---- manifest construction helpers --------------------------------------

/// Base manifest shared by all builtin addons (api 1..=1, pipeline kind).
fn base_manifest(id: &str, name: &str, description: &str) -> Manifest {
    Manifest {
        manifest_version: CURRENT_MANIFEST_VERSION,
        id: id.into(),
        name: name.into(),
        version: "1.0.0".into(),
        author: "static (builtin)".into(),
        description: description.into(),
        license: None,
        homepage: None,
        tags: vec![],
        api_min: 1,
        api_max: 1,
        kind: AddonKind::Pipeline,
        permissions: Default::default(),
        shaders: vec![],
        assets: vec![],
        params: BTreeMap::new(),
        publish: vec![],
        consume: vec![],
    }
}

fn f32_param(default: f32, min: f32, max: f32, label: &str) -> ParamSpec {
    ParamSpec::F32 {
        default,
        min: Some(min),
        max: Some(max),
        ui: UiHints {
            label: Some(label.into()),
            group: None,
            help: None,
        },
    }
}

fn bool_param(default: bool, label: &str) -> ParamSpec {
    ParamSpec::Bool {
        default,
        ui: UiHints {
            label: Some(label.into()),
            group: None,
            help: None,
        },
    }
}
