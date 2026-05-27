//! The SOURCE pass: webcam texture → dot-matrix reconstruction in target 0.
//! It is not a `PostEffect` (it reads the webcam, not a ping target), but it
//! shares the same fullscreen-pass shape so the engine drives it identically.

use wgpu::*;

use super::{fullscreen_pipeline, make_module};

pub struct AsciiSource {
    pipeline: RenderPipeline,
}

impl AsciiSource {
    pub fn new(
        device: &Device,
        globals_layout: &BindGroupLayout,
        input_layout: &BindGroupLayout,
        target_format: TextureFormat,
    ) -> Self {
        let module = make_module(device, "ascii_dot", include_str!("../shaders/ascii_dot.wgsl"));
        let pipeline = fullscreen_pipeline(
            device,
            "ascii_dot",
            &module,
            &[globals_layout, input_layout],
            target_format,
        );
        Self { pipeline }
    }

    pub fn record<'p>(
        &'p self,
        pass: &mut RenderPass<'p>,
        globals_bg: &'p BindGroup,
        input_bg: &'p BindGroup,
    ) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, globals_bg, &[]);
        pass.set_bind_group(1, input_bg, &[]);
        pass.draw(0..3, 0..1);
    }
}
