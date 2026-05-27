//! The shared image-input binding (`@group(1)`): a texture + sampler layout
//! used by any pass that samples an image. The DotRenderer samples the webcam
//! through it.

use wgpu::*;

pub struct ImageBinding {
    pub layout: BindGroupLayout,
    sampler: Sampler,
}

impl ImageBinding {
    pub fn new(device: &Device) -> Self {
        let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("image_input_layout"),
            entries: &[
                BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Texture {
                        multisampled: false,
                        view_dimension: TextureViewDimension::D2,
                        sample_type: TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                BindGroupLayoutEntry {
                    binding: 1,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Sampler(SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&SamplerDescriptor {
            label: Some("image_sampler"),
            mag_filter: FilterMode::Linear,
            min_filter: FilterMode::Linear,
            mipmap_filter: FilterMode::Nearest,
            address_mode_u: AddressMode::ClampToEdge,
            address_mode_v: AddressMode::ClampToEdge,
            ..Default::default()
        });

        Self { layout, sampler }
    }

    /// Build an image-input bind group for `view`.
    pub fn bind_group(&self, device: &Device, view: &TextureView) -> BindGroup {
        device.create_bind_group(&BindGroupDescriptor {
            label: Some("image_bind_group"),
            layout: &self.layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(view),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }
}
