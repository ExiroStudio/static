//! GPU video texture: a persistent texture that webcam frames are streamed
//! into. Created once at the camera's resolution and reused every frame
//! (`write_texture` updates contents in place — no per-frame allocation).

use wgpu::*;

pub struct VideoTexture {
    pub view: TextureView,
    texture: Texture,
    width: u32,
    height: u32,
}

impl VideoTexture {
    pub fn new(device: &Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("webcam_video_texture"),
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let view = texture.create_view(&TextureViewDescriptor::default());

        Self {
            view,
            texture,
            width,
            height,
        }
    }

    /// Upload one tightly-packed RGBA8 frame (natural `4 * width` stride).
    pub fn upload(&self, queue: &Queue, rgba: &[u8]) {
        queue.write_texture(
            ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            rgba,
            ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * self.width),
                rows_per_image: Some(self.height),
            },
            Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }
}
