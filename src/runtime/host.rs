//! The host context uniform (`@group(0)`): resolution + time, shared by every
//! pipeline node. Owned by the runtime and refreshed once per frame — addons
//! read it but never write it.

use wgpu::*;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HostRaw {
    resolution: [f32; 2],
    time: f32,
    _pad: f32,
}

pub struct HostUniform {
    buffer: Buffer,
    pub layout: BindGroupLayout,
    pub bind_group: BindGroup,
}

impl HostUniform {
    pub fn new(device: &Device) -> Self {
        let buffer = device.create_buffer(&BufferDescriptor {
            label: Some("host_uniform"),
            size: std::mem::size_of::<HostRaw>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("host_layout"),
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
            label: Some("host_bind_group"),
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

    pub fn upload(&self, queue: &Queue, resolution: [f32; 2], time: f32) {
        let raw = HostRaw {
            resolution,
            time,
            _pad: 0.0,
        };
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(&raw));
    }
}
