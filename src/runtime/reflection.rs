use crate::runtime::packing::LayoutPlan;
use wgpu::{VertexAttribute, VertexBufferLayout, VertexFormat, VertexStepMode};

/// Dynamically builds a `wgpu::VertexBufferLayout` from a purely semantic `LayoutPlan`.
/// 
/// This is explicitly separated from both the Addon logic (which shouldn't know
/// about `wgpu` vertex states) and the Packing logic (which shouldn't import `wgpu`).
/// It sits in the runtime reflection layer, converting semantic offsets into physical
/// vertex attributes.
pub struct VertexPacker {
    attributes: Vec<VertexAttribute>,
    stride: u64,
}

impl VertexPacker {
    /// Reflects over a compiled `LayoutPlan` to produce tightly packed vertex attributes.
    pub fn new(plan: &LayoutPlan) -> Self {
        let attributes: Vec<VertexAttribute> = plan
            .fields()
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let format = match f.size() {
                    4 => VertexFormat::Float32,
                    8 => VertexFormat::Float32x2,
                    12 => VertexFormat::Float32x3,
                    16 => VertexFormat::Float32x4,
                    _ => VertexFormat::Float32,
                };
                VertexAttribute {
                    format,
                    offset: f.offset() as wgpu::BufferAddress,
                    shader_location: i as u32,
                }
            })
            .collect();

        Self {
            attributes,
            stride: plan.stride() as u64,
        }
    }

    /// Returns the `wgpu::VertexBufferLayout` for use in a `RenderPipelineDescriptor`.
    /// The `VertexPacker` must outlive the pipeline descriptor since the layout borrows
    /// the `attributes` slice.
    pub fn layout(&self) -> VertexBufferLayout {
        VertexBufferLayout {
            array_stride: self.stride,
            step_mode: VertexStepMode::Instance,
            attributes: &self.attributes,
        }
    }
}
