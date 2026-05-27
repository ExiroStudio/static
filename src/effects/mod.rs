//! Shader helpers for fullscreen passes.
//!
//! Composes the shared WGSL prelude (`common.wgsl`) in front of a fragment
//! shader and builds a standard fullscreen-triangle pipeline. Used by the
//! built-in DotRenderer; future addon passes can reuse the same helpers.

pub mod ascii;

use wgpu::*;

/// Shared shader prelude (globals + bindings + vertex stage + helpers),
/// concatenated in front of every fragment shader — the tiny shader composer.
const COMMON: &str = include_str!("../shaders/common.wgsl");

pub fn make_module(device: &Device, label: &str, frag_src: &str) -> ShaderModule {
    let source = format!("{COMMON}\n{frag_src}");
    device.create_shader_module(ShaderModuleDescriptor {
        label: Some(label),
        source: ShaderSource::Wgsl(source.into()),
    })
}

/// Build a standard fullscreen-triangle pipeline (3 verts, no vertex buffer).
pub fn fullscreen_pipeline(
    device: &Device,
    label: &str,
    module: &ShaderModule,
    bind_group_layouts: &[&BindGroupLayout],
    target_format: TextureFormat,
) -> RenderPipeline {
    let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts,
        push_constant_ranges: &[],
    });

    device.create_render_pipeline(&RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: VertexState {
            module,
            entry_point: "vs_main",
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(FragmentState {
            module,
            entry_point: "fs_main",
            targets: &[Some(ColorTargetState {
                format: target_format,
                blend: Some(BlendState::REPLACE),
                write_mask: ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        multiview: None,
    })
}

#[cfg(test)]
mod shader_tests {
    /// Validate the composed shader with naga (wgpu's compiler) — no GPU needed.
    fn validate(name: &str, frag_src: &str) {
        let source = format!("{}\n{}", super::COMMON, frag_src);
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|e| panic!("[{name}] WGSL parse error:\n{}", e.emit_to_string(&source)));
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(&module)
            .unwrap_or_else(|e| panic!("[{name}] WGSL validation error: {e:?}"));
    }

    #[test]
    fn all_shaders_compile() {
        validate("ascii_dot", include_str!("../shaders/ascii_dot.wgsl"));
    }
}
