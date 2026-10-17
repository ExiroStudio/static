use std::time::Duration;
use crate::output::types::{BackendConfig, PixelFormat, ColorSpace, MemoryLocation, FrameRequestKey, OutputBackend};
use crate::output::distribution::FrameDistributionService;
use crate::output::scheduler::OutputScheduler;
use crate::output::backends::linux_v4l2::V4l2Backend as PlatformBackend;

pub struct VirtualCameraSink {
    backend: PlatformBackend,
    target_fps: f32,
    resolution: (u32, u32),
}

impl VirtualCameraSink {
    pub fn new(width: u32, height: u32, target_fps: f32) -> Self {
        let config = BackendConfig {
            width,
            height,
            format: PixelFormat::Nv12,
        };
        let backend = PlatformBackend::new(&config).expect("Failed to init virtual camera");
        
        Self { backend, target_fps, resolution: (width, height) }
    }

    pub fn start(&mut self, scheduler: &mut OutputScheduler) {
        scheduler.register_sink("virtual_camera", self.target_fps);
        self.backend.start().unwrap();
    }

    pub fn render(
        &mut self, 
        current_time: f64, 
        frame_epoch: u64, 
        scheduler: &mut OutputScheduler, 
        distribution: &mut FrameDistributionService
    ) {
        if scheduler.should_render("virtual_camera", current_time) {
            let key = FrameRequestKey {
                resolution: self.resolution,
                pixel_format: PixelFormat::Nv12,
                color_space: ColorSpace::Bt709,
                memory_type: MemoryLocation::Cpu,
                frame_epoch,
            };

            let packet = distribution.request_frame(key);
            self.backend.submit_frame(packet, Duration::from_secs_f64(current_time)).unwrap();
        }
    }
}
