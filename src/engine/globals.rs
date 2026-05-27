//! The engine's small uniform block (`@group(0)`), read by the DotRenderer.
//! Mirrors the WGSL `Globals` struct in `common.wgsl` field-for-field
//! (8 × f32 = 32 bytes, no padding).

use wgpu::*;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Globals {
    pub resolution: [f32; 2],
    pub cell_size: f32,
    pub dot_softness: f32,
    pub contrast: f32,
    pub exposure: f32,
    pub glow: f32,
    pub mirror: f32,
}

impl Default for Globals {
    fn default() -> Self {
        Self {
            resolution: [1.0, 1.0],
            cell_size: 6.0,
            dot_softness: 0.35,
            contrast: 1.40,
            exposure: 1.00,
            glow: 0.50,
            mirror: 1.00,
        }
    }
}

/// GPU-side wrapper: the uniform buffer plus its bind group and layout, bound at
/// `@group(0)` by every pipeline.
pub struct GlobalsGpu {
    pub buffer: Buffer,
    pub layout: BindGroupLayout,
    pub bind_group: BindGroup,
}

impl GlobalsGpu {
    pub fn new(device: &Device) -> Self {
        let buffer = device.create_buffer(&BufferDescriptor {
            label: Some("globals_buffer"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("globals_layout"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("globals_bind_group"),
            layout: &layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        Self {
            buffer,
            layout,
            bind_group,
        }
    }

    pub fn upload(&self, queue: &Queue, globals: &Globals) {
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(globals));
    }
}
