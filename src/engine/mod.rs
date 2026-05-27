//! The core engine: a minimal realtime signal runtime.
//!
//! Pipeline: webcam → DotRenderer → present. Nothing else lives in the core —
//! effects, overlays, tracking and behavior are intended to be external modular
//! addons, not baked into the renderer.

pub mod globals;
pub mod gpu;
pub mod image;
pub mod video;

use std::sync::Arc;

use wgpu::*;
use winit::window::Window;

use crate::camera::WebcamCapture;
use crate::effects::ascii::AsciiSource;

use globals::{Globals, GlobalsGpu};
use gpu::GpuContext;
use image::ImageBinding;
use video::VideoTexture;

pub struct Engine {
    gpu: GpuContext,
    globals_gpu: GlobalsGpu,
    globals: Globals,

    video: VideoTexture,
    webcam_input_bg: BindGroup,

    /// The one built-in render node: luminance → dot matrix, drawn to the surface.
    source: AsciiSource,

    frame_buf: Vec<u8>,
}

impl Engine {
    pub async fn new(window: Arc<Window>, cam_width: u32, cam_height: u32) -> Self {
        let gpu = GpuContext::new(window).await;

        // `image` only needs to live long enough to build the pipeline + bind
        // group; wgpu refcounts the layout/sampler internally afterwards.
        let image = ImageBinding::new(&gpu.device);
        let globals_gpu = GlobalsGpu::new(&gpu.device);
        let mut globals = Globals::default();
        globals.resolution = [gpu.config.width as f32, gpu.config.height as f32];

        let video = VideoTexture::new(&gpu.device, cam_width, cam_height);
        let webcam_input_bg = image.bind_group(&gpu.device, &video.view);

        // DotRenderer draws straight to the swapchain surface (single pass).
        let source = AsciiSource::new(
            &gpu.device,
            &globals_gpu.layout,
            &image.layout,
            gpu.config.format,
        );

        Self {
            gpu,
            globals_gpu,
            globals,
            video,
            webcam_input_bg,
            source,
            frame_buf: Vec::new(),
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.gpu.resize(width, height);
        self.globals.resolution = [self.gpu.config.width as f32, self.gpu.config.height as f32];
    }

    pub fn render(&mut self, camera: &WebcamCapture) {
        // Stream the newest webcam frame to the GPU.
        if camera.copy_latest(&mut self.frame_buf) && !self.frame_buf.is_empty() {
            self.video.upload(&self.gpu.queue, &self.frame_buf);
        }

        self.globals.resolution = [self.gpu.config.width as f32, self.gpu.config.height as f32];
        self.globals_gpu.upload(&self.gpu.queue, &self.globals);

        let frame = match self.gpu.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(SurfaceError::Lost | SurfaceError::Outdated) => {
                self.gpu.surface.configure(&self.gpu.device, &self.gpu.config);
                return;
            }
            Err(_) => return,
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());

        let mut encoder = self.gpu.device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("frame_encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("dot_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
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
            self.source
                .record(&mut pass, &self.globals_gpu.bind_group, &self.webcam_input_bg);
        }
        self.gpu.queue.submit(Some(encoder.finish()));
        frame.present();
    }
}
