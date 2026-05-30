//! Off-screen ping-pong render targets.
//!
//! The runtime keeps two of these and alternates between them so each node can
//! sample the previous node's output while rendering into the other. They carry
//! both `RENDER_ATTACHMENT` (written as a node's output) and `TEXTURE_BINDING`
//! (sampled as the next node's input) usage — the frame never leaves the GPU.

use wgpu::*;

pub struct RenderTarget {
    pub view: TextureView,
    #[allow(dead_code)]
    texture: Texture,
}

impl RenderTarget {
    pub fn new(device: &Device, format: TextureFormat, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("pipeline_target"),
            size: Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format,
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&TextureViewDescriptor::default());
        Self { view, texture }
    }
}
