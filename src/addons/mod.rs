//! Builtin addons — addons that ship inside the engine binary.
//!
//! Each lives in its own file, declares its [`Manifest`] in code, and builds a
//! [`PipelineNode`] from resolved config. They are registered through
//! [`PipelineRuntime::register_builtin`](crate::runtime::PipelineRuntime::register_builtin)
//! and from then on are indistinguishable from external addons — the runtime
//! never names them.
//!
//! [`Manifest`]: crate::addon::manifest::Manifest
//! [`PipelineNode`]: crate::runtime::PipelineNode

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
use crate::runtime::{params_bind_group, params_layout, FrameContext, PipelineNode};

/// The shared shape of a builtin addon node: one fullscreen pass with a params
/// uniform at `@group(2)`. DotRenderer and CRT differ only in their shader and
/// the bytes of that uniform, so the GPU plumbing lives here once.
struct ShaderNode {
    label: &'static str,
    pipeline: RenderPipeline,
    params_bg: BindGroup,
    // Kept alive for as long as the bind group references it.
    _params_buf: Buffer,
}

impl PipelineNode for ShaderNode {
    fn process(&self, ctx: &mut FrameContext) {
        let mut pass = ctx.encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some(self.label),
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, ctx.host_bg, &[]); // host context
        pass.set_bind_group(1, ctx.input_bg, &[]); // frame input
        pass.set_bind_group(2, &self.params_bg, &[]); // addon params
        pass.draw(0..3, 0..1);
    }
}

/// Build a single-pass shader node: upload `params` to a uniform buffer, compose
/// `shader_src` with the shared prelude, and bind `[host, image, params]`.
fn build_shader_node<P: bytemuck::Pod>(
    device: &Device,
    host_layout: &BindGroupLayout,
    image_layout: &BindGroupLayout,
    format: TextureFormat,
    label: &'static str,
    shader_src: &str,
    params: P,
) -> Box<dyn PipelineNode> {
    let params_buf = device.create_buffer_init(&util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::bytes_of(&params),
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
    });
    let plyt = params_layout(device, label);
    let params_bg = params_bind_group(device, &plyt, &params_buf);

    let module = make_module(device, label, shader_src);
    let pipeline = fullscreen_pipeline(
        device,
        label,
        &module,
        &[host_layout, image_layout, &plyt],
        format,
    );

    Box::new(ShaderNode {
        label,
        pipeline,
        params_bg,
        _params_buf: params_buf,
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
