//! The window sink: presents the pipeline's final frame to the swapchain.
//!
//! Sinks are engine-shipped, not addons. v1 ships only the window sink, which
//! is a single fullscreen blit from the pipeline's last output into the surface
//! texture. Keeping it a separate pass means no pipeline node ever has to know
//! about — or render directly to — the swapchain.

use wgpu::*;

pub struct WindowSink {
    pipeline: RenderPipeline,
}

impl WindowSink {
    /// `input_layout` is the runtime's image bind-group layout (texture +
    /// sampler); the final frame is bound through it at `@group(0)`.
    pub fn new(device: &Device, input_layout: &BindGroupLayout, format: TextureFormat) -> Self {
        let module = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("blit"),
            source: ShaderSource::Wgsl(include_str!("../shaders/blit.wgsl").into()),
        });

        let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("blit_layout"),
            bind_group_layouts: &[input_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&layout),
            vertex: VertexState {
                module: &module,
                entry_point: "vs_main",
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(FragmentState {
                module: &module,
                entry_point: "fs_main",
                targets: &[Some(ColorTargetState {
                    format,
                    blend: Some(BlendState::REPLACE),
                    write_mask: ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview: None,
        });

        Self { pipeline }
    }

    /// Blit `input_bg` (the final frame) into `target` (the surface view).
    pub fn blit(&self, encoder: &mut CommandEncoder, input_bg: &BindGroup, target: &TextureView) {
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: Some("sink_blit"),
            color_attachments: &[Some(RenderPassColorAttachment {
                view: target,
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
        pass.set_bind_group(0, input_bg, &[]);
        pass.draw(0..3, 0..1);
    }
}
