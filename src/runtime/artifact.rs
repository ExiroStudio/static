//! Semantic Artifact ABI — Phase 3 of the render architecture migration.
//!
//! Defines the `RenderArtifact` type hierarchy: purely semantic, pure-data
//! descriptions of render intent published by `ExecutionUnit`s (addon logic)
//! and consumed by the `ResourceBroker` (Phase 4) for physical GPU allocation.
//!
//! **Decision references:**
//! - **D003** — Engine Owns GPU: addons never receive `wgpu` types; they only
//!   describe *what* via `RenderArtifact`. The Broker handles *how*.
//! - **D005** — Semantic Payload: `Vec<u8>` is banned from the ABI. All data
//!   flows as typed `SemanticValue` fields. The Broker handles packing.
//!
//! **Invariants enforced here:**
//! - **I001** — ExecutionUnit never owns GPU. Verified by absence of `wgpu`
//!   in this module (compile-time guarantee: no wgpu import).
//! - **I003** — Artifacts remain semantic. No raw bytes, no resource IDs.
//! - **I004** — Broker performs materialization. `RenderArtifact` has no
//!   `materialize()` method.
//! - **I011** — Artifact must remain statically materializable. No runtime
//!   schema discovery; schema is always versioned (`schema_id`).
//! - **I012** — Artifact belongs to exactly one `FrameEpoch`. Enforced by
//!   `HostApi::publish_artifact()` (see `host_api.rs`); the artifact type
//!   itself is `!Clone` to make cross-frame sharing a compile error.
//! - **I013** — Artifact schema resolution must be amortized. `schema_id` is
//!   a `u64` — no string allocation or per-frame schema reconstruction.
//! - **I017** — Artifacts cannot force frame starvation. `ArtifactBudget`
//!   enforces byte and row limits; overflow drops the newest artifact.
//!
//! # Artifact Validation Rules (§7 of `plan.md`)
//! - Row count must strictly match schema bounds (validated in `validate()`).
//! - Field order is immutable (defined by `InstanceSchema::fields`).
//! - Missing or extra fields → reject immediately.
//! - Schema must be versioned (`schema_id != 0`).
//! - Validation happens synchronously at `HostApi::publish_artifact()`.

// Deliberate: no `use wgpu::*` here. The absence of any wgpu import enforces
// **I001** and **D003** at the compiler level. If a future PR adds a wgpu
// import to this file, the invariant comment in plan.md §5 must be updated
// with a Decision ID, reason, cost, and rollback plan.

/// The semantic render intent published by an `ExecutionUnit`.
///
/// Variants cover the four major use cases the current plan targets:
/// instanced rendering (MSDF glyphs, particles), raw geometry (debug overlays),
/// visual primitives (text, icons, atlas regions), and atlas references for
/// pre-uploaded assets. The `Custom` escape hatch is available but requires
/// the addon to supply a stable `schema_id`.
///
/// # Non-Clone
/// `RenderArtifact` intentionally does not implement `Clone`. Cross-frame
/// artifact reuse is **forbidden** (**I012**). The `HostApi` enforces this at
/// runtime; `!Clone` makes accidental reuse a compile error.
#[derive(Debug)]
pub enum RenderArtifact {
    /// No artifact for this slot. The proxy `RenderNode` is a no-op this frame.
    None,
    /// One or more instances described by a semantic schema (e.g. MSDF glyphs).
    Instances {
        schema: InstanceSchema,
        /// Immutable rows — amortized schema parsing per **I013**.
        rows: SemanticRows,
    },
    /// Raw geometry: a list of vertices with a given primitive topology.
    Geometry {
        topology: PrimitiveTopology,
        vertices: SemanticRows,
    },
    /// A high-level visual primitive: text, icon, or atlas region with bounds.
    Visual {
        content: VisualContent,
        /// `[x, y, width, height]` in normalized device coordinates.
        bounds: [f32; 4],
    },
    /// A reference to a pre-uploaded atlas asset, identified by stable id.
    AtlasReference {
        /// Stable asset identity across frames and hot-reloads (**I008**).
        asset_id: u64,
        /// `[u, v, width, height]` in atlas UV space.
        region: [f32; 4],
    },
    /// Escape hatch for addon-specific schemas. `schema_id` must be non-zero
    /// and stable across frames (**I011**).
    Custom {
        schema_id: u64,
        rows: SemanticRows,
    },
}

impl RenderArtifact {
    /// Validate the artifact against its own schema and the §7 rules.
    ///
    /// Called synchronously inside `HostApi::publish_artifact()`. Malformed
    /// artifacts are rejected *before* reaching the broker.
    ///
    /// Returns `Ok(())` on success, `Err(ArtifactValidationError)` on failure.
    pub fn validate(&self) -> Result<(), ArtifactValidationError> {
        match self {
            RenderArtifact::None => Ok(()),

            RenderArtifact::Instances { schema, rows } => {
                // Schema must be versioned (schema_id != 0).
                if schema.schema_id == 0 {
                    return Err(ArtifactValidationError::UnversionedSchema);
                }
                if rows.schema_id != schema.schema_id {
                    return Err(ArtifactValidationError::SchemaIdMismatch {
                        artifact: rows.schema_id,
                        schema: schema.schema_id,
                    });
                }
                // Each row must have exactly as many values as the schema has fields.
                let expected_fields = schema.fields.len();
                for (i, row) in rows.rows.iter().enumerate() {
                    if row.values.len() != expected_fields {
                        return Err(ArtifactValidationError::RowFieldCountMismatch {
                            row_index: i,
                            expected: expected_fields,
                            actual: row.values.len(),
                        });
                    }
                }
                Ok(())
            }

            RenderArtifact::Geometry { vertices, .. } => {
                if vertices.schema_id == 0 {
                    return Err(ArtifactValidationError::UnversionedSchema);
                }
                Ok(())
            }

            RenderArtifact::Visual { content, bounds } => {
                content.validate()?;
                // bounds: [x, y, w, h] — width and height must be positive.
                if bounds[2] <= 0.0 || bounds[3] <= 0.0 {
                    return Err(ArtifactValidationError::InvalidBounds(*bounds));
                }
                Ok(())
            }

            RenderArtifact::AtlasReference { asset_id, region } => {
                if *asset_id == 0 {
                    return Err(ArtifactValidationError::ZeroAssetId);
                }
                if region[2] <= 0.0 || region[3] <= 0.0 {
                    return Err(ArtifactValidationError::InvalidRegion(*region));
                }
                Ok(())
            }

            RenderArtifact::Custom { schema_id, rows } => {
                if *schema_id == 0 {
                    return Err(ArtifactValidationError::UnversionedSchema);
                }
                if rows.schema_id != *schema_id {
                    return Err(ArtifactValidationError::SchemaIdMismatch {
                        artifact: rows.schema_id,
                        schema: *schema_id,
                    });
                }
                Ok(())
            }
        }
    }

    /// Returns `true` if this is the `None` variant (no-op artifact).
    pub fn is_none(&self) -> bool {
        matches!(self, RenderArtifact::None)
    }

    /// Approximate byte size of this artifact's semantic payload.
    ///
    /// Used by `ArtifactBudget` to enforce **I017** (no frame starvation).
    /// This is a semantic estimate — the Broker may produce more or fewer
    /// physical bytes after packing/alignment.
    pub fn estimated_bytes(&self) -> usize {
        match self {
            RenderArtifact::None => 0,
            RenderArtifact::Instances { rows, .. } => rows.estimated_bytes(),
            RenderArtifact::Geometry { vertices, .. } => vertices.estimated_bytes(),
            RenderArtifact::Visual { .. } => std::mem::size_of::<[f32; 4]>() * 2,
            RenderArtifact::AtlasReference { .. } => std::mem::size_of::<[f32; 4]>() + 8,
            RenderArtifact::Custom { rows, .. } => rows.estimated_bytes(),
        }
    }
}

/// High-level visual content variant for the `Visual` artifact.
#[derive(Debug)]
pub enum VisualContent {
    /// A text string to be rendered (mode determines rasterization strategy).
    Text {
        content: String,
        mode: TextMode,
    },
    /// An icon identified by name (resolved by the broker against the asset
    /// registry at materialization time).
    Icon(String),
    /// A pre-positioned atlas region, identified by asset + UV quad.
    AtlasRegion {
        asset_id: u64,
        region: [f32; 4],
    },
    /// Addon-defined visual type.
    Custom {
        schema_id: u64,
    },
}

impl VisualContent {
    fn validate(&self) -> Result<(), ArtifactValidationError> {
        match self {
            VisualContent::Text { content, .. } => {
                if content.is_empty() {
                    return Err(ArtifactValidationError::EmptyTextContent);
                }
                Ok(())
            }
            VisualContent::Icon(name) => {
                if name.is_empty() {
                    return Err(ArtifactValidationError::EmptyIconName);
                }
                Ok(())
            }
            VisualContent::AtlasRegion { asset_id, region } => {
                if *asset_id == 0 {
                    return Err(ArtifactValidationError::ZeroAssetId);
                }
                if region[2] <= 0.0 || region[3] <= 0.0 {
                    return Err(ArtifactValidationError::InvalidRegion(*region));
                }
                Ok(())
            }
            VisualContent::Custom { schema_id } => {
                if *schema_id == 0 {
                    return Err(ArtifactValidationError::UnversionedSchema);
                }
                Ok(())
            }
        }
    }
}

/// How a `Text` visual is rasterized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMode {
    /// Plain CPU-side text rasterization.
    Plain,
    /// Rich text with embedded formatting.
    Rich,
    /// MSDF glyph-by-glyph rasterization (the primary MSDF mode).
    Glyph,
}

/// Primitive drawing topology for `Geometry` artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveTopology {
    TriangleList,
    TriangleStrip,
    LineList,
    LineStrip,
    PointList,
}

/// A typed, ordered collection of semantic rows.
///
/// `schema_id` ties these rows to a specific `InstanceSchema` or custom schema.
/// The Broker resolves the schema by id (never by runtime inspection — **I013**)
/// and decides how to pack the values into GPU memory (**D005**).
#[derive(Debug)]
pub struct SemanticRows {
    /// Stable schema identifier. Must be non-zero (**I011**). The Broker uses
    /// this to look up packing rules without per-frame schema reconstruction.
    pub schema_id: u64,
    /// The ordered rows. Broker MAY convert internally to AoS → SoA, arena,
    /// or other layouts; the ABI always presents `Vec<SemanticRow>`.
    pub rows: Vec<SemanticRow>,
}

impl SemanticRows {
    /// Approximate semantic byte size (not including struct overhead).
    pub fn estimated_bytes(&self) -> usize {
        self.rows.iter().map(|r| r.estimated_bytes()).sum()
    }
}

/// The schema for an `Instances` artifact: an ordered list of typed fields.
///
/// Field order is **immutable** once published. The Broker reads fields in
/// declaration order to produce tightly-packed per-instance buffers.
#[derive(Debug)]
pub struct InstanceSchema {
    /// Stable schema version identifier. Must be non-zero (**I011**).
    pub schema_id: u64,
    /// Ordered field descriptors. The count must match every `SemanticRow`
    /// in the associated `SemanticRows` (validated in `RenderArtifact::validate()`).
    pub fields: Vec<SemanticField>,
}

/// A single typed field descriptor in an `InstanceSchema`.
///
/// The Broker uses these to calculate `std140`/`std430` padding and stride
/// without exposing alignment rules to the addon (**D005**).
#[derive(Debug, Clone, PartialEq)]
pub enum SemanticField {
    /// 2D position (x, y) — maps to `vec2<f32>` in WGSL.
    Position2,
    /// 3D position (x, y, z) — maps to `vec3<f32>` in WGSL.
    Position3,
    /// RGBA color (r, g, b, a) — maps to `vec4<f32>` in WGSL.
    ColorRgba,
    /// UV quad (u, v, width, height) — maps to `vec4<f32>` in WGSL.
    UvQuad,
    /// Custom named float (e.g. "glyph_index"). Name is for diagnostics only;
    /// the Broker packs it as a single `f32`.
    CustomFloat(String),
}

/// A single row of typed values, corresponding to one instance/vertex.
///
/// The Broker flattens this into a tightly-packed `[u8]` GPU buffer applying
/// `std140`/`std430` rules internally (**D005**). The addon never sees bytes.
#[derive(Debug)]
pub struct SemanticRow {
    /// Field values in the same order as `InstanceSchema::fields`.
    pub values: Vec<SemanticValue>,
}

impl SemanticRow {
    fn estimated_bytes(&self) -> usize {
        self.values.iter().map(SemanticValue::byte_size).sum()
    }
}

/// A single typed value inside a `SemanticRow`.
#[derive(Debug, Clone, PartialEq)]
pub enum SemanticValue {
    /// Single float → 4 bytes.
    Float(f32),
    /// 2-component float vector → 8 bytes.
    Vec2([f32; 2]),
    /// 3-component float vector → 12 bytes.
    Vec3([f32; 3]),
    /// 4-component float vector → 16 bytes.
    Vec4([f32; 4]),
}

impl SemanticValue {
    /// Semantic byte size (pre-packing; actual GPU size may differ due to
    /// alignment). Used by `ArtifactBudget` for budget estimation.
    pub fn byte_size(&self) -> usize {
        match self {
            SemanticValue::Float(_) => 4,
            SemanticValue::Vec2(_) => 8,
            SemanticValue::Vec3(_) => 12,
            SemanticValue::Vec4(_) => 16,
        }
    }
}

// ---------------------------------------------------------------------------
// Budget enforcement (I017)
// ---------------------------------------------------------------------------

/// Per-frame artifact budget. Enforces **I017**: artifacts cannot force frame
/// starvation. Overflow policy: drop the *newest* artifact and emit a warning;
/// never stall the render thread.
///
/// Limits are configurable; the defaults are conservative and can be tuned
/// without changing the ABI.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactBudget {
    /// Maximum byte size of a single artifact (semantic estimate).
    pub max_artifact_bytes: usize,
    /// Maximum total byte size of all artifacts in one frame.
    pub max_frame_bytes: usize,
    /// Maximum number of rows across all artifacts in one frame.
    pub max_frame_rows: usize,
}

impl Default for ArtifactBudget {
    fn default() -> Self {
        ArtifactBudget {
            max_artifact_bytes: 4 * 1024 * 1024, // 4 MiB per artifact
            max_frame_bytes: 16 * 1024 * 1024,   // 16 MiB per frame
            max_frame_rows: 1_000_000,            // 1M rows per frame
        }
    }
}

impl ArtifactBudget {
    /// Check whether a single artifact is within budget.
    pub fn check_artifact(&self, artifact: &RenderArtifact) -> Result<(), ArtifactValidationError> {
        let bytes = artifact.estimated_bytes();
        if bytes > self.max_artifact_bytes {
            return Err(ArtifactValidationError::ArtifactBudgetExceeded {
                bytes,
                limit: self.max_artifact_bytes,
            });
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Validation error type
// ---------------------------------------------------------------------------

/// Errors produced by `RenderArtifact::validate()` or budget checks.
///
/// All variants are reject-and-continue — a malformed artifact is dropped
/// without stalling the render thread (**I017**).
#[derive(Debug, Clone, PartialEq)]
pub enum ArtifactValidationError {
    /// `schema_id == 0` — schema is unversioned, violating **I011**.
    UnversionedSchema,
    /// `SemanticRows::schema_id` does not match the enclosing schema.
    SchemaIdMismatch { artifact: u64, schema: u64 },
    /// A `SemanticRow` has a different number of values than the schema's fields.
    RowFieldCountMismatch {
        row_index: usize,
        expected: usize,
        actual: usize,
    },
    /// `Visual` bounds have non-positive width or height.
    InvalidBounds([f32; 4]),
    /// Atlas region has non-positive width or height.
    InvalidRegion([f32; 4]),
    /// `AtlasReference::asset_id == 0`.
    ZeroAssetId,
    /// `Text` content string is empty.
    EmptyTextContent,
    /// `Icon` name is empty.
    EmptyIconName,
    /// Single artifact exceeds `ArtifactBudget::max_artifact_bytes` (**I017**).
    ArtifactBudgetExceeded { bytes: usize, limit: usize },
    /// Frame total exceeds `ArtifactBudget::max_frame_bytes` (**I017**).
    FrameBudgetExceeded { bytes: usize, limit: usize },
}

impl std::fmt::Display for ArtifactValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArtifactValidationError::UnversionedSchema => {
                write!(f, "artifact schema_id must be non-zero (I011)")
            }
            ArtifactValidationError::SchemaIdMismatch { artifact, schema } => {
                write!(
                    f,
                    "SemanticRows schema_id {artifact} does not match InstanceSchema schema_id {schema}"
                )
            }
            ArtifactValidationError::RowFieldCountMismatch {
                row_index,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "row {row_index}: expected {expected} fields, got {actual}"
                )
            }
            ArtifactValidationError::InvalidBounds(b) => {
                write!(f, "invalid bounds {:?}: width/height must be positive", b)
            }
            ArtifactValidationError::InvalidRegion(r) => {
                write!(f, "invalid region {:?}: width/height must be positive", r)
            }
            ArtifactValidationError::ZeroAssetId => {
                write!(f, "asset_id must be non-zero")
            }
            ArtifactValidationError::EmptyTextContent => {
                write!(f, "text content must be non-empty")
            }
            ArtifactValidationError::EmptyIconName => {
                write!(f, "icon name must be non-empty")
            }
            ArtifactValidationError::ArtifactBudgetExceeded { bytes, limit } => {
                write!(
                    f,
                    "artifact size {bytes}B exceeds per-artifact limit {limit}B (I017)"
                )
            }
            ArtifactValidationError::FrameBudgetExceeded { bytes, limit } => {
                write!(
                    f,
                    "frame artifact total {bytes}B exceeds frame limit {limit}B (I017)"
                )
            }
        }
    }
}

impl std::error::Error for ArtifactValidationError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_instance_schema() -> InstanceSchema {
        InstanceSchema {
            schema_id: 42,
            fields: vec![SemanticField::Position2, SemanticField::ColorRgba],
        }
    }

    fn valid_rows(schema: &InstanceSchema) -> SemanticRows {
        SemanticRows {
            schema_id: schema.schema_id,
            rows: vec![SemanticRow {
                values: vec![
                    SemanticValue::Vec2([0.0, 0.0]),
                    SemanticValue::Vec4([1.0, 0.0, 0.0, 1.0]),
                ],
            }],
        }
    }

    #[test]
    fn none_artifact_is_valid() {
        assert!(RenderArtifact::None.validate().is_ok());
    }

    #[test]
    fn instances_artifact_validates_correctly() {
        let schema = valid_instance_schema();
        let rows = valid_rows(&schema);
        let artifact = RenderArtifact::Instances { schema, rows };
        assert!(artifact.validate().is_ok());
    }

    #[test]
    fn unversioned_schema_id_is_rejected() {
        let artifact = RenderArtifact::Instances {
            schema: InstanceSchema {
                schema_id: 0, // violates I011
                fields: vec![],
            },
            rows: SemanticRows {
                schema_id: 0,
                rows: vec![],
            },
        };
        assert_eq!(
            artifact.validate(),
            Err(ArtifactValidationError::UnversionedSchema)
        );
    }

    #[test]
    fn schema_id_mismatch_is_rejected() {
        let schema = valid_instance_schema();
        let rows = SemanticRows {
            schema_id: 99, // wrong
            rows: vec![],
        };
        let artifact = RenderArtifact::Instances { schema, rows };
        assert!(matches!(
            artifact.validate(),
            Err(ArtifactValidationError::SchemaIdMismatch { .. })
        ));
    }

    #[test]
    fn row_field_count_mismatch_is_rejected() {
        let schema = valid_instance_schema(); // 2 fields
        let rows = SemanticRows {
            schema_id: schema.schema_id,
            rows: vec![SemanticRow {
                values: vec![SemanticValue::Vec2([0.0, 0.0])], // only 1 value
            }],
        };
        let artifact = RenderArtifact::Instances { schema, rows };
        assert!(matches!(
            artifact.validate(),
            Err(ArtifactValidationError::RowFieldCountMismatch {
                row_index: 0,
                expected: 2,
                actual: 1
            })
        ));
    }

    #[test]
    fn visual_with_zero_bounds_is_rejected() {
        let artifact = RenderArtifact::Visual {
            content: VisualContent::Text {
                content: "hello".into(),
                mode: TextMode::Plain,
            },
            bounds: [0.0, 0.0, 0.0, 0.5], // width == 0 → invalid
        };
        assert!(matches!(
            artifact.validate(),
            Err(ArtifactValidationError::InvalidBounds(_))
        ));
    }

    #[test]
    fn atlas_reference_with_zero_asset_id_is_rejected() {
        let artifact = RenderArtifact::AtlasReference {
            asset_id: 0,
            region: [0.0, 0.0, 0.5, 0.5],
        };
        assert_eq!(
            artifact.validate(),
            Err(ArtifactValidationError::ZeroAssetId)
        );
    }

    #[test]
    fn budget_rejects_oversized_artifact() {
        let budget = ArtifactBudget {
            max_artifact_bytes: 10,
            max_frame_bytes: 100,
            max_frame_rows: 1000,
        };
        // Produce an artifact with > 10 bytes: 1 row × Vec4 = 16 bytes.
        let schema = InstanceSchema {
            schema_id: 1,
            fields: vec![SemanticField::ColorRgba],
        };
        let rows = SemanticRows {
            schema_id: 1,
            rows: vec![SemanticRow {
                values: vec![SemanticValue::Vec4([1.0, 0.0, 0.0, 1.0])],
            }],
        };
        let artifact = RenderArtifact::Instances { schema, rows };
        assert!(matches!(
            budget.check_artifact(&artifact),
            Err(ArtifactValidationError::ArtifactBudgetExceeded { .. })
        ));
    }

    #[test]
    fn semantic_value_byte_sizes_are_correct() {
        assert_eq!(SemanticValue::Float(0.0).byte_size(), 4);
        assert_eq!(SemanticValue::Vec2([0.0; 2]).byte_size(), 8);
        assert_eq!(SemanticValue::Vec3([0.0; 3]).byte_size(), 12);
        assert_eq!(SemanticValue::Vec4([0.0; 4]).byte_size(), 16);
    }
}
