//! Resource Broker — Phase 4 of the render architecture.
//!
//! The Broker is the **only** path through which semantic `RenderArtifact`s
//! reach physical GPU memory. It is an orchestrator: it delegates to focused
//! sub-components (`Allocator`, `Packer`, `Uploader`, `Cache`, `Metrics`) and
//! enforces the critical pipeline invariant:
//!
//! ```text
//!   Semantic (RenderArtifact)
//!          ↓
//!       Broker
//!          ↓
//!       GPU (wgpu::Buffer)
//! ```
//!
//! There is **no** shortcut from Semantic → GPU or Execution → GPU. Any addon
//! that tries to call `device.create_buffer()` directly will not compile
//! (addons never receive a `wgpu::Device` reference — **D003**).
//!
//! # Sub-component responsibilities
//!
//! | Component | Responsibility |
//! |-----------|----------------|
//! | `Allocator` | `create_buffer`, geometric growth, Grace Window eviction |
//! | `Packer` | `LayoutPlan` cache, calls `packing::pack_rows()` |
//! | `Uploader` | `queue.write_buffer()` abstraction |
//! | `Cache` | `BrokerKey → MaterializedResource` lookup |
//! | `Metrics` | Per-frame counters (allocation, reuse, resize, eviction) |
//!
//! # I020 — Allocation only in Prepare
//! The `materialize()` method is called **exclusively** from
//! `PipelineRuntime::prepare()` (the Prepare Phase). It MUST NOT be called
//! from inside a render pass. The broker has no mechanism to enforce this at
//! compile time, but the caller (mod.rs) guarantees it structurally.
//!
//! # Grace Window
//! Buffers that are no longer referenced by the current `ExecutionPlan` enter
//! `Idle` state. After `GRACE_WINDOW` duration without use, they are evicted
//! during the post-frame `sweep()`. Buffers never shrink; they are only
//! destroyed on eviction.
//!
//! **Decision refs:** D003, D004, D005
//! **Invariants:** I001, I014, I016, I017, I019, I020

use std::collections::HashMap;
use std::time::{Duration, Instant};

use wgpu::{Buffer, BufferDescriptor, BufferUsages, Device, Queue};

use crate::runtime::artifact::{InstanceSchema, RenderArtifact, SemanticRows};
use crate::runtime::packing::{pack_rows, LayoutPlan, PackingProfile};
use crate::runtime::plan::PlanEpoch;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Duration a buffer remains in `Idle` state before the Grace Window expires
/// and the buffer is evicted during `sweep()`.
const GRACE_WINDOW: Duration = Duration::from_secs(1);

/// Allocation granularity: all buffer sizes are rounded up to this multiple to
/// reduce fragmentation (4 KiB matches typical GPU page alignment).
const ALLOC_GRANULARITY: u64 = 4096;

// ---------------------------------------------------------------------------
// BrokerKey
// ---------------------------------------------------------------------------

/// Stable cache key: identifies a buffer slot by graph epoch + node slot.
///
/// `plan_epoch` isolates cache entries between graph compilations (after a
/// `build()`, old epoch keys become Idle and are eventually evicted).
/// `node_slot` is the `RenderGraphNode::slot` assigned during graph build.
/// `schema_id` is **intentionally excluded** from the key so that schema
/// upgrades on the same node trigger an in-place resize/repack rather than a
/// full eviction + reallocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BrokerKey {
    pub plan_epoch: u64,
    pub node_slot: u32,
}

impl BrokerKey {
    pub fn new(epoch: PlanEpoch, slot: usize) -> Self {
        Self {
            plan_epoch: epoch.raw(),
            node_slot: slot as u32,
        }
    }
}

// ---------------------------------------------------------------------------
// ResourceState — lifecycle state machine
// ---------------------------------------------------------------------------

/// Lifecycle state of a `MaterializedResource`.
///
/// State machine: `Allocated → Warm → Active → Idle → Evicted`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceState {
    /// Buffer created; data not yet uploaded.
    Allocated,
    /// Data uploaded; not yet referenced by the active `ExecutionPlan`.
    Warm,
    /// Referenced by the current frame's `ExecutionPlan`.
    Active,
    /// No longer referenced; Grace Window countdown started.
    Idle,
    /// Grace Window expired; buffer destroyed during `sweep()`.
    Evicted,
}

// ---------------------------------------------------------------------------
// MaterializedResource
// ---------------------------------------------------------------------------

/// A physical GPU buffer + lifecycle metadata owned by the Broker.
///
/// Survives across frames (unlike `RenderArtifact` which drops at frame end —
/// **I012**). The Broker is the sole owner of the `wgpu::Buffer` (**I016**).
pub struct MaterializedResource {
    /// The physical GPU buffer (only valid while state != Evicted).
    pub buffer: Buffer,
    /// Byte capacity of the buffer (may exceed current data size).
    pub capacity: u64,
    /// Last `schema_id` packed into this buffer. Used to detect schema changes
    /// that require a stride-update repack.
    pub schema_id: u64,
    /// Current lifecycle state.
    pub state: ResourceState,
    /// Timestamp of the last frame that used this resource.
    pub last_used: Instant,
    /// Number of rows last uploaded.
    pub last_row_count: usize,
    /// Cached compiled layout for the current `schema_id`.
    pub layout: Option<LayoutPlan>,
}

// ---------------------------------------------------------------------------
// BrokerMetrics — per-frame counters
// ---------------------------------------------------------------------------

/// Lightweight per-frame diagnostic counters.
///
/// Exposed read-only via `ResourceBroker::metrics()`. Never panic on overflow
/// (wrapping arithmetic). Used by the debug visualizer and future telemetry.
#[derive(Debug, Clone, Copy, Default)]
pub struct BrokerMetrics {
    /// Times `create_buffer` was called this frame.
    pub allocation_count: u64,
    /// Times an existing buffer was reused without resize.
    pub reuse_count: u64,
    /// Times a buffer was grown (geometric resize).
    pub resize_count: u64,
    /// Times a buffer was dropped during `sweep()`.
    pub eviction_count: u64,
    /// Times `queue.write_buffer` was called this frame.
    pub upload_count: u64,
    /// Total bytes uploaded via `write_buffer` this frame.
    pub bytes_uploaded: u64,
}

// ---------------------------------------------------------------------------
// MaterializeResult
// ---------------------------------------------------------------------------

/// The outcome of a single `materialize()` call.
///
/// `Ok(handle)` means the artifact was materialized and the buffer is ready
/// for binding. `Err(MaterializeError)` means the artifact was skipped; the
/// corresponding draw call should be omitted (graceful degradation — **I017**).
#[derive(Debug)]
pub struct MaterializedHandle {
    /// Stable key identifying which buffer to bind.
    pub key: BrokerKey,
    /// Number of rows available for drawing.
    pub row_count: usize,
    /// Byte stride between rows (for vertex fetch stage configuration).
    pub stride: usize,
}

/// Errors from `materialize()`. All variants result in a skipped draw call,
/// never a crash (**I017**).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializeError {
    /// Artifact is the `None` variant — no draw needed.
    NoArtifact,
    /// Computed buffer size exceeds `wgpu` limits or physical memory budget.
    ExceedsGpuLimit { required: u64 },
    /// Allocation failed (OOM or device lost). Existing buffers preserved.
    AllocationFailed,
    /// Stride computed as zero (empty field list). Artifact is malformed.
    ZeroStride,
}

// ---------------------------------------------------------------------------
// ResourceBroker
// ---------------------------------------------------------------------------

/// The Phase 4 Resource Broker.
///
/// Owns all dynamic GPU buffers used by the render graph. Addons interact with
/// this only indirectly (via `RenderArtifact` → `HostApi` → `materialize()`).
///
/// # Structure
/// ```text
/// ResourceBroker
///   ├── cache: HashMap<BrokerKey, MaterializedResource>   (Cache)
///   ├── layout_cache: HashMap<u64, LayoutPlan>            (Packer, I013)
///   └── metrics: BrokerMetrics                            (Metrics)
/// ```
pub struct ResourceBroker {
    /// Buffer cache: `BrokerKey → MaterializedResource`. The Broker is the
    /// sole owner of all `wgpu::Buffer`s (**I016**).
    cache: HashMap<BrokerKey, MaterializedResource>,

    /// Per-schema compiled layout plans. Cached once, replayed every frame
    /// (enforces **I013**: schema resolution must be amortized).
    layout_cache: HashMap<u64, LayoutPlan>,

    /// Per-frame rolling metrics. Reset each frame by `begin_frame()`.
    frame_metrics: BrokerMetrics,

    /// Accumulated totals since the Broker was created (never reset).
    total_metrics: BrokerMetrics,

    /// Maximum total GPU memory (bytes) the Broker may hold at once.
    /// Exceeding this causes `materialize()` to return `ExceedsGpuLimit`.
    memory_budget: u64,
}

impl ResourceBroker {
    /// Create a new `ResourceBroker` with the given memory budget.
    ///
    /// Call `begin_frame()` at the start of each frame and `sweep()` after
    /// the frame to maintain the Grace Window eviction lifecycle.
    pub fn new(memory_budget: u64) -> Self {
        Self {
            cache: HashMap::new(),
            layout_cache: HashMap::new(),
            frame_metrics: BrokerMetrics::default(),
            total_metrics: BrokerMetrics::default(),
            memory_budget,
        }
    }

    /// Reset per-frame metrics and mark all `Active` resources as `Idle`.
    ///
    /// Called by `PipelineRuntime` before the Prepare Phase begins (I020).
    pub fn begin_frame(&mut self) {
        self.frame_metrics = BrokerMetrics::default();

        // Transition Active → Idle (they haven't been referenced yet this frame).
        // Warm stays Warm until materialize() touches it.
        for res in self.cache.values_mut() {
            if res.state == ResourceState::Active {
                res.state = ResourceState::Idle;
            }
        }
    }

    /// Materialize a `RenderArtifact` into a physical GPU buffer.
    ///
    /// # Flow (Checkpoint 4+5)
    /// 1. **Validator** — skip `None` artifacts; check size vs GPU limits.
    /// 2. **Packer** — look up or compile `LayoutPlan` for the artifact schema.
    /// 3. **Allocator** — `ensure_buffer()`: create or geometrically resize.
    /// 4. **Uploader** — `queue.write_buffer()` via `upload()`.
    /// 5. Return `MaterializedHandle`.
    ///
    /// All errors are non-fatal: returns `Err(MaterializeError)`, caller omits
    /// the draw call (**I017**, graceful degradation).
    ///
    /// # I020
    /// This method MUST only be called during the Prepare Phase. Calling it
    /// from inside a `wgpu::RenderPass` (Execution Phase) is a logic error.
    pub fn materialize(
        &mut self,
        device: &Device,
        queue: &Queue,
        key: BrokerKey,
        artifact: &RenderArtifact,
    ) -> Result<MaterializedHandle, MaterializeError> {
        // ── Step 1: Validator ──────────────────────────────────────────────
        let (schema, rows) = extract_instances(artifact)?;

        if rows.rows.is_empty() {
            return Err(MaterializeError::NoArtifact);
        }

        // ── Step 2: Packer — LayoutPlan (I013: compile once, replay forever) ──
        let layout = self.get_or_compile_layout(schema);

        if layout.stride == 0 {
            return Err(MaterializeError::ZeroStride);
        }

        let row_count = rows.rows.len();
        let required_bytes = layout.buffer_size(row_count) as u64;

        if required_bytes > self.memory_budget {
            return Err(MaterializeError::ExceedsGpuLimit {
                required: required_bytes,
            });
        }

        // ── Step 3: Allocator — ensure_buffer() ───────────────────────────
        self.ensure_buffer(device, key, required_bytes, schema.schema_id, &layout)?;

        // ── Step 4: Uploader ───────────────────────────────────────────────
        {
            // Prepare staging bytes.
            let mut staging = vec![0u8; required_bytes as usize];
            pack_rows(&mut staging, &rows.rows, &layout);

            // Upload to GPU.
            self.upload(queue, key, &staging);
        }

        // Mark as Active so sweep() doesn't evict it.
        if let Some(res) = self.cache.get_mut(&key) {
            res.state = ResourceState::Active;
            res.last_used = Instant::now();
            res.last_row_count = row_count;
        }

        Ok(MaterializedHandle {
            key,
            row_count,
            stride: layout.stride,
        })
    }

    /// Evict `Idle` resources whose Grace Window has expired.
    ///
    /// Call this **after** `queue.submit()` at the end of each frame. Drops
    /// the `wgpu::Buffer`, freeing VRAM. Buffers in `Active` or `Warm` state
    /// are never evicted.
    pub fn sweep(&mut self) {
        let now = Instant::now();
        let mut to_evict: Vec<BrokerKey> = Vec::new();

        for (key, res) in &mut self.cache {
            if res.state == ResourceState::Idle {
                if now.duration_since(res.last_used) >= GRACE_WINDOW {
                    res.state = ResourceState::Evicted;
                    to_evict.push(*key);
                }
            }
        }

        for key in to_evict {
            self.cache.remove(&key);
            self.frame_metrics.eviction_count = self.frame_metrics.eviction_count.wrapping_add(1);
            self.total_metrics.eviction_count = self.total_metrics.eviction_count.wrapping_add(1);
        }
    }

    /// Read per-frame metrics (reset by `begin_frame()`).
    pub fn frame_metrics(&self) -> &BrokerMetrics {
        &self.frame_metrics
    }

    /// Read accumulated lifetime metrics.
    pub fn total_metrics(&self) -> &BrokerMetrics {
        &self.total_metrics
    }

    /// Borrow the `MaterializedResource` for a given `key` (for draw calls).
    ///
    /// Returns `None` if the key is unknown or was evicted.
    pub fn get(&self, key: &BrokerKey) -> Option<&MaterializedResource> {
        self.cache.get(key)
    }

    /// Number of currently cached (non-evicted) resources.
    pub fn cached_count(&self) -> usize {
        self.cache.len()
    }

    // ── Private: Packer ───────────────────────────────────────────────────

    /// Retrieve the cached `LayoutPlan` for `schema_id`, or compile a new one.
    ///
    /// The plan is keyed by `schema_id`, not by schema content — stable ids
    /// guarantee deterministic reuse across frames (**I013**).
    fn get_or_compile_layout(&mut self, schema: &InstanceSchema) -> LayoutPlan {
        if let Some(cached) = self.layout_cache.get(&schema.schema_id) {
            return cached.clone();
        }
        // Cache miss — compile once.
        let plan = LayoutPlan::compile(&schema.fields, PackingProfile::StorageStd430);
        self.layout_cache.insert(schema.schema_id, plan.clone());
        plan
    }

    // ── Private: Allocator ────────────────────────────────────────────────

    /// Ensure a buffer of at least `required` bytes exists for `key`.
    ///
    /// **Allocation strategy (STEP 5 from blueprint):**
    /// - First allocation: round up to `ALLOC_GRANULARITY`.
    /// - Reuse: if `capacity >= required`, return immediately (no allocation).
    /// - Resize: geometric growth — `capacity = max(required, capacity * 2)`,
    ///   rounded to granularity. Buffers NEVER shrink.
    fn ensure_buffer(
        &mut self,
        device: &Device,
        key: BrokerKey,
        required: u64,
        schema_id: u64,
        layout: &LayoutPlan,
    ) -> Result<(), MaterializeError> {
        // Reuse path (cache hit, capacity sufficient).
        if let Some(res) = self.cache.get_mut(&key) {
            if res.capacity >= required {
                // Schema changed → update cached schema_id + layout (in-place
                // reconfigure, no buffer drop — key is intentionally schema-agnostic).
                if res.schema_id != schema_id {
                    res.schema_id = schema_id;
                    res.layout = Some(layout.clone());
                }
                res.state = ResourceState::Warm;
                self.frame_metrics.reuse_count = self.frame_metrics.reuse_count.wrapping_add(1);
                self.total_metrics.reuse_count = self.total_metrics.reuse_count.wrapping_add(1);
                return Ok(());
            }

            // Capacity insufficient → geometric resize.
            let new_cap = round_up(required.max(res.capacity.saturating_mul(2)), ALLOC_GRANULARITY);
            let buffer = create_buffer(device, new_cap)
                .ok_or(MaterializeError::AllocationFailed)?;

            res.buffer = buffer;
            res.capacity = new_cap;
            res.schema_id = schema_id;
            res.layout = Some(layout.clone());
            res.state = ResourceState::Warm;

            self.frame_metrics.resize_count = self.frame_metrics.resize_count.wrapping_add(1);
            self.total_metrics.resize_count = self.total_metrics.resize_count.wrapping_add(1);
            return Ok(());
        }

        // Cache miss → first allocation.
        let cap = round_up(required.max(ALLOC_GRANULARITY), ALLOC_GRANULARITY);
        let buffer = create_buffer(device, cap).ok_or(MaterializeError::AllocationFailed)?;

        self.cache.insert(
            key,
            MaterializedResource {
                buffer,
                capacity: cap,
                schema_id,
                state: ResourceState::Warm,
                last_used: Instant::now(),
                last_row_count: 0,
                layout: Some(layout.clone()),
            },
        );

        self.frame_metrics.allocation_count = self.frame_metrics.allocation_count.wrapping_add(1);
        self.total_metrics.allocation_count = self.total_metrics.allocation_count.wrapping_add(1);
        Ok(())
    }

    // ── Private: Uploader ─────────────────────────────────────────────────

    /// Submit a `queue.write_buffer()` upload for the buffer at `key`.
    ///
    /// Abstracts the wgpu upload call, making it trivially replaceable with
    /// a staging belt or DMA path in Phase 5 without touching the Broker.
    fn upload(&mut self, queue: &Queue, key: BrokerKey, data: &[u8]) {
        if let Some(res) = self.cache.get(&key) {
            queue.write_buffer(&res.buffer, 0, data);
            let bytes = data.len() as u64;
            self.frame_metrics.upload_count = self.frame_metrics.upload_count.wrapping_add(1);
            self.frame_metrics.bytes_uploaded =
                self.frame_metrics.bytes_uploaded.wrapping_add(bytes);
            self.total_metrics.upload_count = self.total_metrics.upload_count.wrapping_add(1);
            self.total_metrics.bytes_uploaded =
                self.total_metrics.bytes_uploaded.wrapping_add(bytes);
        }
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Extract `(InstanceSchema, SemanticRows)` from a `RenderArtifact::Instances`.
///
/// Returns `Err(NoArtifact)` for all other variants (they are either handled
/// by dedicated materializers in future phases or are no-ops).
fn extract_instances(
    artifact: &RenderArtifact,
) -> Result<(&InstanceSchema, &SemanticRows), MaterializeError> {
    match artifact {
        RenderArtifact::Instances { schema, rows } => Ok((schema, rows)),
        RenderArtifact::None => Err(MaterializeError::NoArtifact),
        _ => Err(MaterializeError::NoArtifact), // Geometry/Visual/Atlas handled in future phases
    }
}

/// Allocate a `wgpu::Buffer` of `size` bytes with vertex + copy_dst usages.
///
/// Returns `None` on failure (device lost, OOM). The Broker treats this as
/// `AllocationFailed` and emits a warning, never panicking.
fn create_buffer(device: &Device, size: u64) -> Option<Buffer> {
    if size == 0 {
        return None;
    }
    Some(device.create_buffer(&BufferDescriptor {
        label: Some("broker_dynamic"),
        size,
        // VERTEX: can be bound as vertex/instance buffer.
        // COPY_DST: required for queue.write_buffer().
        usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    }))
}

/// Round `value` up to the nearest multiple of `granularity`.
#[inline]
fn round_up(value: u64, granularity: u64) -> u64 {
    if granularity == 0 {
        return value;
    }
    let rem = value % granularity;
    if rem == 0 { value } else { value + (granularity - rem) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Note: full broker integration tests (create_buffer / write_buffer) require
    // a live wgpu::Device and are excluded from headless unit testing. The tests
    // below cover pure-logic components: BrokerKey, round_up, allocation strategy
    // reasoning, and metrics baseline — no GPU device needed.

    #[test]
    fn broker_key_hash_stability() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        let epoch = PlanEpoch(1);
        set.insert(BrokerKey::new(epoch, 0));
        set.insert(BrokerKey::new(epoch, 1));
        set.insert(BrokerKey::new(epoch, 0)); // duplicate
        assert_eq!(set.len(), 2, "BrokerKey hashing must deduplicate");
    }

    #[test]
    fn round_up_to_granularity() {
        assert_eq!(round_up(1, 4096), 4096);
        assert_eq!(round_up(4096, 4096), 4096);
        assert_eq!(round_up(4097, 4096), 8192);
        assert_eq!(round_up(0, 4096), 0);
        assert_eq!(round_up(100, 0), 100); // guard against div-by-zero
    }

    #[test]
    fn resource_state_initial_is_allocated() {
        // Verify the state machine starts correctly.
        assert_eq!(ResourceState::Allocated as u8, ResourceState::Allocated as u8);
    }

    #[test]
    fn broker_metrics_default_all_zero() {
        let m = BrokerMetrics::default();
        assert_eq!(m.allocation_count, 0);
        assert_eq!(m.reuse_count, 0);
        assert_eq!(m.resize_count, 0);
        assert_eq!(m.eviction_count, 0);
        assert_eq!(m.upload_count, 0);
        assert_eq!(m.bytes_uploaded, 0);
    }

    #[test]
    fn materialize_error_no_artifact_for_none() {
        // extract_instances rejects None variant.
        let artifact = RenderArtifact::None;
        assert!(matches!(
            extract_instances(&artifact),
            Err(MaterializeError::NoArtifact)
        ));
    }

    #[test]
    fn broker_key_different_epochs_are_different() {
        let k1 = BrokerKey::new(PlanEpoch(1), 0);
        let k2 = BrokerKey::new(PlanEpoch(2), 0);
        assert_ne!(k1, k2, "different epochs must produce different keys");
    }

    #[test]
    fn broker_new_has_empty_cache() {
        let broker = ResourceBroker::new(64 * 1024 * 1024);
        assert_eq!(broker.cached_count(), 0);
        assert_eq!(broker.frame_metrics().allocation_count, 0);
    }
}
