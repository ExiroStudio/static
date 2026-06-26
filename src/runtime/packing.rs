//! Semantic-to-physical packing engine — Phase 4, Resource Broker.
//!
//! Pure layout logic: no `wgpu`, no GPU concepts. The module receives typed
//! `SemanticRow`s and alignment rules, writes bytes into a caller-provided
//! staging buffer, and returns a `PackingResult`. The broker owns the staging
//! memory and the GPU upload; this module only knows about field widths,
//! strides, and padding.
//!
//! # Separation contract
//! - `packing.rs` MUST NOT import `wgpu`. Enforced by the absence of any
//!   `wgpu` in the use-list below (**I014**: Broker materializes, never computes;
//!   corollary: the packing module never allocates GPU memory).
//! - The caller (Broker) decides which `PackingProfile` to use and passes the
//!   compiled `LayoutPlan`. Per **I013**, the plan is computed once and cached;
//!   the packer simply replays it, eliminating per-frame schema branching.
//!
//! **Decision references:** D005, I014
//! **Invariants:** I013, I015, I019

use crate::runtime::artifact::{SemanticField, SemanticRow, SemanticValue};

// ---------------------------------------------------------------------------
// PackingProfile — alignment rule set
// ---------------------------------------------------------------------------

/// The alignment and stride rule set the broker selects for a given artifact.
///
/// Each variant maps to a specific GPU buffer layout. The packing module uses
/// the variant to derive element sizes and inter-element padding without knowing
/// anything about WGSL, `wgpu`, or `std140` by name — just integer alignment.
///
/// Variants ordered by expected usage frequency (MSDF/particles first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackingProfile {
    /// `std430` storage buffer layout. Vec3 is padded to 16 bytes.
    /// Used for large instanced datasets (MSDF glyphs, particles).
    StorageStd430,
    /// Compact packing: no inter-field padding. Vec3 occupies exactly 12 bytes.
    /// Used for CPU-side staging where alignment is irrelevant.
    StorageCompact,
    /// Vertex buffer, one struct per instance. Same alignment as `StorageStd430`
    /// but the stride is used by the vertex fetch stage.
    VertexInstanced,
    /// Interleaved vertex buffer (position + uv + color packed together).
    VertexInterleaved,
    /// Separate vertex buffer (all positions first, then all UVs, etc.).
    /// Note: currently mapped identically to `VertexInstanced`; the distinction
    /// will matter when SoA packing is implemented in Phase 5.
    VertexSeparated,
}

// ---------------------------------------------------------------------------
// LayoutPlan — amortized compiled field plan (I013)
// ---------------------------------------------------------------------------

/// A single field's compiled packing metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldLayout {
    /// Byte size of the field value as written into the buffer.
    pub size: usize,
    /// Alignment of this field within the struct (in bytes).
    pub alignment: usize,
    /// Byte offset of this field from the start of one row.
    pub offset: usize,
}

/// A compiled layout plan for one artifact schema.
///
/// Computed **once** from `InstanceSchema::fields` + `PackingProfile` and
/// cached by the Broker. Replayed each frame without recomputing field offsets
/// or alignments (enforces **I013**: schema resolution must be amortized).
///
/// Immutable after construction — field order is frozen per **plan.md §7**.
#[derive(Debug, Clone)]
pub struct LayoutPlan {
    /// Per-field layout, in schema declaration order.
    pub fields: Vec<FieldLayout>,
    /// Total byte stride from the start of one row to the next (includes
    /// tail-padding to satisfy `profile` alignment requirements).
    pub stride: usize,
}

impl LayoutPlan {
    /// Compile a `LayoutPlan` from a field list and a packing profile.
    ///
    /// This is an **O(n fields)** operation called once per schema id per
    /// broker session. The result is cached and replayed every frame (**I013**).
    ///
    /// The algorithm is deterministic: same fields + same profile → same layout
    /// (**I015**, **I019**).
    pub fn compile(fields: &[SemanticField], profile: PackingProfile) -> Self {
        let mut result = Vec::with_capacity(fields.len());
        let mut cursor: usize = 0;

        for field in fields {
            let (size, alignment) = field_metrics(field, profile);
            // Advance cursor to satisfy alignment.
            let padding = align_padding(cursor, alignment);
            cursor += padding;
            result.push(FieldLayout {
                size,
                alignment,
                offset: cursor,
            });
            cursor += size;
        }

        // Struct tail-padding: the stride must be a multiple of the maximum
        // field alignment so that arrays of structs are correctly aligned.
        let max_align = result.iter().map(|f| f.alignment).max().unwrap_or(1);
        let tail_pad = align_padding(cursor, max_align);
        let stride = cursor + tail_pad;

        LayoutPlan {
            fields: result,
            stride,
        }
    }

    /// Total buffer size required for `row_count` rows.
    pub fn buffer_size(&self, row_count: usize) -> usize {
        self.stride * row_count
    }
}

// ---------------------------------------------------------------------------
// PackingResult
// ---------------------------------------------------------------------------

/// The outcome of a single `pack_rows()` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackingResult {
    /// Number of bytes written into the staging buffer.
    pub bytes_written: usize,
    /// Stride in bytes between the start of each row in the buffer.
    pub stride: usize,
    /// Number of rows successfully packed.
    pub rows_packed: usize,
}

// ---------------------------------------------------------------------------
// Core packing function
// ---------------------------------------------------------------------------

/// Pack `rows` into `dst` using the pre-compiled `plan`.
///
/// # Contract
/// - `dst.len()` MUST be `>= plan.buffer_size(rows.len())`. The caller
///   (Broker) is responsible for ensuring the staging buffer is large enough.
///   Panics in debug mode if the invariant is violated; silently truncates in
///   release (write-past-end is UB so the panic is intentional).
/// - No GPU types are touched here. `dst` is a plain mutable byte slice.
/// - Deterministic: same `rows` + same `plan` → identical bytes (**I019**).
///
/// Returns a `PackingResult` describing what was written.
pub fn pack_rows(dst: &mut [u8], rows: &[SemanticRow], plan: &LayoutPlan) -> PackingResult {
    let required = plan.buffer_size(rows.len());
    debug_assert!(
        dst.len() >= required,
        "staging buffer too small: need {required}B, have {}B",
        dst.len()
    );

    let mut bytes_written = 0usize;

    for (row_idx, row) in rows.iter().enumerate() {
        let row_base = row_idx * plan.stride;

        for (field_idx, (value, layout)) in row.values.iter().zip(plan.fields.iter()).enumerate() {
            let _ = field_idx; // used in debug assertion only
            let dst_start = row_base + layout.offset;
            let dst_end = dst_start + layout.size;

            debug_assert!(
                dst_end <= dst.len(),
                "field {field_idx} of row {row_idx} writes past dst (dst={}B dst_end={dst_end}B)",
                dst.len()
            );

            if dst_end > dst.len() {
                // Release-mode safety: stop packing rather than writing OOB.
                break;
            }

            write_value(&mut dst[dst_start..dst_end], value, layout.size);
        }

        bytes_written = (row_base + plan.stride).min(dst.len());
    }

    PackingResult {
        bytes_written,
        stride: plan.stride,
        rows_packed: rows.len(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns `(byte_size, alignment)` for a `SemanticField` under `profile`.
///
/// This is the **only** place that maps semantic types to byte widths. Changing
/// `std430` padding for `Vec3` only requires a change here.
fn field_metrics(field: &SemanticField, profile: PackingProfile) -> (usize, usize) {
    match field {
        SemanticField::Position2 => (8, 8),    // vec2<f32>: 8 bytes, align 8
        SemanticField::ColorRgba => (16, 16),  // vec4<f32>: 16 bytes, align 16
        SemanticField::UvQuad => (16, 16),     // vec4<f32>: 16 bytes, align 16

        // Vec3 is the tricky one: std430 pads to 16; compact uses 12.
        SemanticField::Position3 => match profile {
            PackingProfile::StorageStd430
            | PackingProfile::VertexInstanced
            | PackingProfile::VertexInterleaved
            | PackingProfile::VertexSeparated => (16, 16), // vec3 padded to vec4
            PackingProfile::StorageCompact => (12, 4),     // tightly packed f32×3
        },

        // CustomFloat is a single f32: 4 bytes, align 4.
        SemanticField::CustomFloat(_) => (4, 4),
    }
}

/// Writes `value` into `dst` as little-endian bytes.
/// `dst.len()` must equal `layout_size` (enforced in debug via `debug_assert`).
fn write_value(dst: &mut [u8], value: &SemanticValue, layout_size: usize) {
    match value {
        SemanticValue::Float(v) => {
            debug_assert_eq!(layout_size, 4);
            dst[..4].copy_from_slice(&v.to_le_bytes());
        }
        SemanticValue::Vec2(v) => {
            debug_assert!(layout_size >= 8);
            dst[..4].copy_from_slice(&v[0].to_le_bytes());
            dst[4..8].copy_from_slice(&v[1].to_le_bytes());
        }
        SemanticValue::Vec3(v) => {
            debug_assert!(layout_size >= 12);
            dst[..4].copy_from_slice(&v[0].to_le_bytes());
            dst[4..8].copy_from_slice(&v[1].to_le_bytes());
            dst[8..12].copy_from_slice(&v[2].to_le_bytes());
            // Pad the 4th component to zero if layout_size == 16 (std430).
            if layout_size == 16 {
                dst[12..16].copy_from_slice(&[0u8; 4]);
            }
        }
        SemanticValue::Vec4(v) => {
            debug_assert_eq!(layout_size, 16);
            for (i, f) in v.iter().enumerate() {
                dst[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
            }
        }
    }
}

/// Compute the number of padding bytes to add at `cursor` to reach `alignment`.
#[inline]
fn align_padding(cursor: usize, alignment: usize) -> usize {
    if alignment == 0 {
        return 0;
    }
    let rem = cursor % alignment;
    if rem == 0 { 0 } else { alignment - rem }
}

// ---------------------------------------------------------------------------
// Tests — Checkpoint 2 (Packing Math Validation)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::artifact::{SemanticField, SemanticRow, SemanticValue};

    fn f32_le(v: f32) -> [u8; 4] {
        v.to_le_bytes()
    }

    /// Build a layout plan + pack a single Position2 field (std430).
    #[test]
    fn position2_packs_8_bytes() {
        let fields = vec![SemanticField::Position2];
        let plan = LayoutPlan::compile(&fields, PackingProfile::StorageStd430);
        assert_eq!(plan.stride, 8);
        assert_eq!(plan.fields[0].offset, 0);
        assert_eq!(plan.fields[0].size, 8);

        let rows = vec![SemanticRow {
            values: vec![SemanticValue::Vec2([1.0, 2.0])],
        }];
        let mut buf = vec![0u8; plan.buffer_size(rows.len())];
        let result = pack_rows(&mut buf, &rows, &plan);

        assert_eq!(result.bytes_written, 8);
        assert_eq!(result.stride, 8);
        assert_eq!(&buf[0..4], &f32_le(1.0));
        assert_eq!(&buf[4..8], &f32_le(2.0));
    }

    /// Vec4 (ColorRgba) — 16 bytes, aligned to 16.
    #[test]
    fn color_rgba_packs_16_bytes() {
        let fields = vec![SemanticField::ColorRgba];
        let plan = LayoutPlan::compile(&fields, PackingProfile::StorageStd430);
        assert_eq!(plan.stride, 16);

        let rows = vec![SemanticRow {
            values: vec![SemanticValue::Vec4([0.1, 0.2, 0.3, 1.0])],
        }];
        let mut buf = vec![0u8; plan.buffer_size(rows.len())];
        pack_rows(&mut buf, &rows, &plan);

        assert_eq!(&buf[0..4], &f32_le(0.1));
        assert_eq!(&buf[4..8], &f32_le(0.2));
        assert_eq!(&buf[8..12], &f32_le(0.3));
        assert_eq!(&buf[12..16], &f32_le(1.0));
    }

    /// Vec3 under std430: padded to 16, zero-padded 4th component.
    #[test]
    fn position3_std430_pads_to_16() {
        let fields = vec![SemanticField::Position3];
        let plan = LayoutPlan::compile(&fields, PackingProfile::StorageStd430);
        assert_eq!(plan.stride, 16, "std430 Vec3 stride must be 16");

        let rows = vec![SemanticRow {
            values: vec![SemanticValue::Vec3([1.0, 2.0, 3.0])],
        }];
        let mut buf = vec![0u8; plan.buffer_size(rows.len())];
        pack_rows(&mut buf, &rows, &plan);

        assert_eq!(&buf[0..4], &f32_le(1.0));
        assert_eq!(&buf[4..8], &f32_le(2.0));
        assert_eq!(&buf[8..12], &f32_le(3.0));
        assert_eq!(&buf[12..16], &[0u8; 4], "4th component must be zero-padded");
    }

    /// Vec3 under compact: 12 bytes, no padding.
    #[test]
    fn position3_compact_is_12_bytes() {
        let fields = vec![SemanticField::Position3];
        let plan = LayoutPlan::compile(&fields, PackingProfile::StorageCompact);
        assert_eq!(plan.stride, 12, "compact Vec3 stride must be 12");
    }

    /// Mixed layout: Position2 + ColorRgba.
    /// Position2 ends at byte 8; ColorRgba aligns to 16 → 8 bytes padding.
    #[test]
    fn mixed_position2_color_layout() {
        let fields = vec![SemanticField::Position2, SemanticField::ColorRgba];
        let plan = LayoutPlan::compile(&fields, PackingProfile::StorageStd430);
        // Position2: offset=0, size=8
        // ColorRgba: align=16 → needs to start at 16, so 8 bytes pad
        assert_eq!(plan.fields[0].offset, 0);
        assert_eq!(plan.fields[1].offset, 16, "ColorRgba must be aligned to 16");
        // Stride = 16 + 16 = 32
        assert_eq!(plan.stride, 32);
    }

    /// Multiple rows are correctly strided.
    #[test]
    fn multiple_rows_are_correctly_strided() {
        let fields = vec![SemanticField::CustomFloat("x".into())];
        let plan = LayoutPlan::compile(&fields, PackingProfile::StorageCompact);
        assert_eq!(plan.stride, 4);

        let rows = vec![
            SemanticRow { values: vec![SemanticValue::Float(1.0)] },
            SemanticRow { values: vec![SemanticValue::Float(2.0)] },
            SemanticRow { values: vec![SemanticValue::Float(3.0)] },
        ];
        let mut buf = vec![0u8; plan.buffer_size(rows.len())];
        let result = pack_rows(&mut buf, &rows, &plan);

        assert_eq!(result.rows_packed, 3);
        assert_eq!(result.stride, 4);
        assert_eq!(&buf[0..4], &f32_le(1.0));
        assert_eq!(&buf[4..8], &f32_le(2.0));
        assert_eq!(&buf[8..12], &f32_le(3.0));
    }

    /// Determinism: same input → identical output bytes (I019).
    #[test]
    fn packing_is_deterministic() {
        let fields = vec![SemanticField::Position2, SemanticField::ColorRgba];
        let plan = LayoutPlan::compile(&fields, PackingProfile::StorageStd430);
        let rows = vec![SemanticRow {
            values: vec![
                SemanticValue::Vec2([0.5, -0.5]),
                SemanticValue::Vec4([1.0, 0.0, 0.5, 1.0]),
            ],
        }];

        let mut buf1 = vec![0u8; plan.buffer_size(rows.len())];
        let mut buf2 = vec![0u8; plan.buffer_size(rows.len())];
        pack_rows(&mut buf1, &rows, &plan);
        pack_rows(&mut buf2, &rows, &plan);

        assert_eq!(buf1, buf2, "packing must be deterministic (I019)");
    }

    /// Same schema + profile → identical LayoutPlan (I015).
    #[test]
    fn layout_plan_is_deterministic() {
        let fields = vec![SemanticField::Position2, SemanticField::ColorRgba];
        let p1 = LayoutPlan::compile(&fields, PackingProfile::StorageStd430);
        let p2 = LayoutPlan::compile(&fields, PackingProfile::StorageStd430);
        assert_eq!(p1.stride, p2.stride);
        assert_eq!(p1.fields.len(), p2.fields.len());
        for (a, b) in p1.fields.iter().zip(p2.fields.iter()) {
            assert_eq!(a, b);
        }
    }

    /// Empty field list produces zero-stride plan (no panic).
    #[test]
    fn empty_fields_produce_zero_stride() {
        let plan = LayoutPlan::compile(&[], PackingProfile::StorageStd430);
        assert_eq!(plan.stride, 0);
        assert!(plan.fields.is_empty());
    }

    /// `buffer_size` matches manual calculation.
    #[test]
    fn buffer_size_is_stride_times_row_count() {
        let fields = vec![SemanticField::ColorRgba]; // stride = 16
        let plan = LayoutPlan::compile(&fields, PackingProfile::StorageStd430);
        assert_eq!(plan.buffer_size(5), 80);
        assert_eq!(plan.buffer_size(0), 0);
    }
}
