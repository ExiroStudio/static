//! GPU context: the device, queue, surface and its configuration. This is the
//! lowest layer — it knows nothing about effects or the render graph.

use std::sync::Arc;

use wgpu::*;
use winit::window::Window;

pub struct GpuContext {
    pub surface: Surface<'static>,
    pub device: Device,
    pub queue: Queue,
    pub config: SurfaceConfiguration,
}

impl GpuContext {
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let instance = Instance::default();

        // `Arc<Window>` yields a `Surface<'static>` that keeps the window alive.
        let surface = instance
            .create_surface(window)
            .expect("failed to create surface");

        let adapter = instance
            .request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("no suitable GPU adapter found");

        let (device, queue) = adapter
            .request_device(
                &DeviceDescriptor {
                    label: Some("device"),
                    required_features: Features::empty(),
                    required_limits: Limits::default(),
                },
                None,
            )
            .await
            .expect("failed to create device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);

        let config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo, // stable, tear-free, OBS-friendly
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        Self {
            surface,
            device,
            queue,
            config,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }
}
