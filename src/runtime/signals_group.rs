//! `@group(3)` — the optional *dynamic bindings* group.
//!
//! Groups 0/1/2 (host / input / static params) are unchanged. `@group(3)` is
//! created only when a filter declares `consume = [...]`; filters that consume
//! nothing never get it, so their pipeline layout is byte-identical to before.
//!
//! Its first (and currently only) use is a per-frame **signals uniform**: each
//! consumed signal is packed into a `vec4<f32>` slot in the filter's declared
//! `consume` order. The group is intentionally generic — future dynamic
//! resources (an emoji atlas, an overlay texture, a history buffer) can be added
//! as additional bindings without changing this contract.

use wgpu::util::DeviceExt;
use wgpu::*;

use crate::signal::{SignalId, SignalSnapshot, SignalValue};

use super::context::SignalContext;

/// The `@group(3)` layout for the signals uniform (binding 0, fragment-visible).
pub fn signals_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some("signals_group3"),
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
    })
}

/// One consumed signal occupies a 16-byte (`vec4<f32>`) slot — the std140 rule
/// that keeps every element aligned with no per-kind packing logic.
const SLOT_BYTES: usize = 16;

/// A filter's `@group(3)` signals uniform: the resolved consume ids plus the
/// GPU buffer/bind group, updated each frame via `write_buffer`.
pub struct SignalsBinding {
    buffer: Buffer,
    bind_group: BindGroup,
    /// Resolved ids in declared `consume` order; `None` = optional & unpublished.
    ids: Vec<Option<SignalId>>,
    /// Reused staging buffer (no per-frame allocation).
    scratch: Vec<u8>,
}

impl SignalsBinding {
    /// Build the binding for a filter, or `None` if it consumes nothing (then
    /// the filter has no `@group(3)` at all).
    pub fn new(device: &Device, layout: &BindGroupLayout, signals: &SignalContext) -> Option<Self> {
        let refs = signals.consume();
        if refs.is_empty() {
            return None;
        }
        let ids: Vec<Option<SignalId>> = refs.iter().map(|r| signals.id(&r.name)).collect();
        let bytes = ids.len() * SLOT_BYTES;

        let buffer = device.create_buffer_init(&util::BufferInitDescriptor {
            label: Some("signals_group3"),
            contents: &vec![0u8; bytes],
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        });
        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("signals_group3"),
            layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });

        Some(Self {
            buffer,
            bind_group,
            ids,
            scratch: vec![0u8; bytes],
        })
    }

    pub fn bind_group(&self) -> &BindGroup {
        &self.bind_group
    }

    /// Pack the latest snapshot of the consumed signals into the uniform — one
    /// `vec4<f32>` per consumed signal, in declared order. No allocation.
    pub fn update(&mut self, queue: &Queue, snapshot: &SignalSnapshot) {
        for (slot, id) in self.ids.iter().enumerate() {
            let v = id.map(|id| as_vec4(snapshot.get(id))).unwrap_or([0.0; 4]);
            let off = slot * SLOT_BYTES;
            self.scratch[off..off + SLOT_BYTES].copy_from_slice(bytemuck::bytes_of(&v));
        }
        queue.write_buffer(&self.buffer, 0, &self.scratch);
    }
}

/// Widen any signal value into a `vec4<f32>` for the uniform slot.
fn as_vec4(value: SignalValue) -> [f32; 4] {
    match value {
        SignalValue::Bool(b) => [b as u32 as f32, 0.0, 0.0, 0.0],
        SignalValue::F32(x) => [x, 0.0, 0.0, 0.0],
        SignalValue::I32(i) => [i as f32, 0.0, 0.0, 0.0],
        SignalValue::Vec2([a, b]) => [a, b, 0.0, 0.0],
        SignalValue::Vec3([a, b, c]) => [a, b, c, 0.0],
        SignalValue::Vec4(v) => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_packs_into_a_vec4_slot() {
        assert_eq!(as_vec4(SignalValue::F32(0.5)), [0.5, 0.0, 0.0, 0.0]);
        assert_eq!(as_vec4(SignalValue::Bool(true)), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(as_vec4(SignalValue::I32(7)), [7.0, 0.0, 0.0, 0.0]);
        assert_eq!(as_vec4(SignalValue::Vec2([1.0, 2.0])), [1.0, 2.0, 0.0, 0.0]);
        assert_eq!(as_vec4(SignalValue::Vec3([1.0, 2.0, 3.0])), [1.0, 2.0, 3.0, 0.0]);
        assert_eq!(
            as_vec4(SignalValue::Vec4([1.0, 2.0, 3.0, 4.0])),
            [1.0, 2.0, 3.0, 4.0]
        );
        // Each slot is exactly 16 bytes (std140 vec4 alignment).
        assert_eq!(SLOT_BYTES, std::mem::size_of::<[f32; 4]>());
    }
}
