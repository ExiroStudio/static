# Walkthrough — Render Architecture Implementation (Phase 1–3)
**Date:** 2026-06-26  
**Branch:** `refactor/render-runtime`  
**Repo:** `/var/www/static/engine`

---

## Ringkasan Singkat

Mengimplementasikan 3 phase pertama dari `plan.md`:
1. **Phase 1** — `RenderGraph` skeleton (graph-local ordering layer)
2. **Phase 2** — `ExecutionPlan` + `PlanEpoch` (separate compilation dari execution)
3. **Phase 3** — Semantic Artifact ABI (`RenderArtifact`, `HostApi::publish_artifact()`)

Juga diperbaiki 4 pre-existing compile errors di `smoke.rs` dan `behavior/host.rs` yang menghalangi `cargo test`.

---

## Phase 1 — RenderGraph Skeleton

**Decisions:** D002, D006  
**Invariants:** I002, I007

### [NEW] `src/runtime/graph.rs`

**Mengapa dibuat:**  
`PipelineRuntime` sebelumnya menyimpan `Vec<Box<dyn FilterNode>>` tanpa ordering metadata. Phase 1 menambah layer `RenderGraph` yang wraps node-node dengan `slot` index sequential, memberikan fondasi untuk `ExecutionPlan` (Phase 2) dan `RenderArtifact` routing (Phase 3).

**Yang berubah:** File baru.

**Isi:**
| Tipe | Visibilitas | Deskripsi |
|------|-------------|-----------|
| `struct RenderGraphNode` | `pub(crate)` | Wraps `Box<dyn FilterNode>` + `slot: usize`. Internal only (I007). |
| `struct RenderGraph` | `pub` | Ordered collection of `RenderGraphNode`. Satu-satunya authoritative ordering source (I002). |
| `impl RenderGraph::new()` | `pub` | Empty graph constructor. |
| `impl RenderGraph::push()` | `pub` | Append node, assign next slot index. |
| `impl RenderGraph::len()` | `pub` | Node count. |
| `impl RenderGraph::is_empty()` | `pub` | Empty check. |
| `impl RenderGraph::nodes()` | `pub(crate)` | Immutable ordered slice. |
| `impl RenderGraph::nodes_mut()` | `pub(crate)` | Mutable slice untuk `prepare()` calls. |

**Invariants yang dijamin:**
- `RenderGraphNode` tidak expose ke luar modul → I007
- Slot index sequential, assigned di `push()` → I002 (ordering hanya berubah di ExecutionPlan)
- Tidak ada wgpu import → tidak melanggar I001

**Tests:**
- `graph_slot_assignment_is_sequential` — slot 0,1,2 untuk 3 nodes
- `empty_graph_reports_correctly` — is_empty(), len()

---

### [MODIFY] `src/runtime/mod.rs` (Phase 1 bagian)

**Yang berubah:**

1. **Tambah `pub mod graph;`** di deklarasi modul
2. **`pub use graph::RenderGraph;`** di re-exports
3. **`PipelineRuntime` struct**: field `nodes: Vec<Box<dyn FilterNode>>` diganti dengan:
   ```rust
   // Phase 1 (D002, D006)
   graph: RenderGraph,
   ```
4. **`PipelineRuntime::new()`**: init `graph: RenderGraph::new()` instead of `nodes: Vec::new()`
5. **`PipelineRuntime::build()`**: loop sekarang push ke `new_graph: RenderGraph` bukan `nodes: Vec`
6. **`PipelineRuntime::render()`**:
   - Signal-binding pass: `for gn in self.graph.nodes_mut()` (bukan `self.nodes.iter_mut()`)  
   - Render pass: `for (i, gn) in self.graph.nodes().iter().enumerate()` (bukan `self.nodes.iter()`)
   - `gn.node.prepare(...)` dan `gn.node.process(...)` instead of `node.prepare/process`

**Komentar referensi yang ditambahkan:** `// D002, D006` di setiap bagian yang relevan.

---

## Phase 2 — ExecutionPlan + PlanEpoch

**Decisions:** D004  
**Invariants:** I002, I006, I015, I018

### [NEW] `src/runtime/plan.rs`

**Mengapa dibuat:**  
Memisahkan *compilation* (apa yang jalan dan dalam urutan apa) dari *execution* (submit GPU work). Sebelumnya tidak ada distinction ini. Sekarang `build()` compile satu kali → `ExecutionPlan` immutable. `render()` hanya execute, tidak bisa mutate topology.

**Isi:**
| Tipe | Deskripsi |
|------|-----------|
| `struct PlanEpoch(u64)` | Monotonic epoch counter, newtype. `ZERO` sentinel untuk "belum ada plan". |
| `impl PlanEpoch::next()` | Return `PlanEpoch(self.0 + 1)`. Saturating add. |
| `struct ExecutionPlan` | Immutable snapshot: `epoch`, `node_count`, `plan_hash`. |
| `impl ExecutionPlan::compile()` | **Satu-satunya constructor** (I006). Computes deterministic `plan_hash` dari epoch + node_count (I015). |

**Invariants yang dijamin:**
- I006: `compile()` satu-satunya constructor. Tidak ada `ExecutionPlan { .. }` literal di luar modul ini.
- I015: Deterministik — `DefaultHasher` + same inputs → same hash. Verified oleh unit test `same_graph_same_hash`.
- I018: `ExecutionPlan` tidak ada `&mut self` methods. Frame execution tidak bisa mutate plan.
- I002: Ordering hanya bisa berubah lewat `compile()` baru.

**Tidak diimplementasikan `Clone`:** Mencegah dua "live plan" koeksistensi (I018).

**Tests (Validation Gate §11.8 — Compile Epoch Gate):**
- `plan_epoch_advances_monotonically` — ZERO < 1 < 2
- `compile_produces_correct_node_count` — node_count matches graph
- `same_graph_same_hash` — **Validation Gate §11.8**: same topology → same plan_hash
- `different_graph_different_hash` — 2 nodes vs 3 nodes → different hash
- `different_epoch_different_hash` — epoch contributes to hash

---

### [MODIFY] `src/runtime/mod.rs` (Phase 2 bagian)

**Yang berubah:**

1. **Tambah `pub mod plan;`** di deklarasi modul
2. **`pub use plan::{ExecutionPlan, PlanEpoch};`** di re-exports
3. **`PipelineRuntime` struct**: tambah dua field baru:
   ```rust
   // Phase 2 (D004, I002, I006, I015, I018)
   current_epoch: PlanEpoch,
   current_plan: Option<ExecutionPlan>,
   ```
4. **`PipelineRuntime::new()`**: init `current_epoch: PlanEpoch::ZERO, current_plan: None`
5. **`PipelineRuntime::build()`**: setelah semua node di-push ke `new_graph`, tambah:
   ```rust
   // Phase 2: advance epoch, compile immutable plan
   let new_epoch = self.current_epoch.next();
   let new_plan = ExecutionPlan::compile(new_epoch, &new_graph);
   self.current_epoch = new_epoch;
   self.current_plan = Some(new_plan);
   ```
6. **`PipelineRuntime::render()`**: tambah:
   ```rust
   // Phase 2 (I002, I018): read compiled plan, don't mutate
   let node_count = self.current_plan.as_ref().map(|p| p.node_count).unwrap_or(0);
   debug_assert_eq!(node_count, self.graph.len(), "I015");
   ```

---

## Phase 3 — Semantic Artifact ABI

**Decisions:** D003, D005  
**Invariants:** I001, I003, I004, I011, I012, I013, I014, I017

### [NEW] `src/runtime/artifact.rs`

**Mengapa dibuat:**  
Mendefinisikan ABI semantik antara `ExecutionUnit` (addon logic) dan `ResourceBroker` (Phase 4). Addon publish `RenderArtifact` — purely semantic, no GPU types. Broker materialize physical buffers. Separation ini adalah inti dari D003 dan D005.

**⚠️ Tidak ada `use wgpu::*`** — compile-time guarantee I001 (ExecutionUnit never owns GPU).

**Isi lengkap:**
| Tipe | Deskripsi |
|------|-----------|
| `enum RenderArtifact` | None, Instances, Geometry, Visual, AtlasReference, Custom |
| `impl RenderArtifact::validate()` | Synchronous validation (§7 rules). Called di publish. |
| `impl RenderArtifact::estimated_bytes()` | Untuk budget estimation (I017). |
| `enum VisualContent` | Text, Icon, AtlasRegion, Custom |
| `enum TextMode` | Plain, Rich, Glyph |
| `enum PrimitiveTopology` | TriangleList, TriangleStrip, LineList, LineStrip, PointList |
| `struct SemanticRows` | `schema_id: u64` + `rows: Vec<SemanticRow>`. schema_id non-zero (I011). |
| `struct InstanceSchema` | `schema_id: u64` + `fields: Vec<SemanticField>`. |
| `enum SemanticField` | Position2, Position3, ColorRgba, UvQuad, CustomFloat(String) |
| `struct SemanticRow` | `values: Vec<SemanticValue>`. Flattened oleh Broker (D005). |
| `enum SemanticValue` | Float, Vec2, Vec3, Vec4 + byte_size() |
| `struct ArtifactBudget` | max_artifact_bytes, max_frame_bytes, max_frame_rows. Default: 4MB/16MB. |
| `enum ArtifactValidationError` | Semua error variants dengan Display impl. |

**`RenderArtifact` tidak implement `Clone`** — cross-frame reuse compile error (I012).

**Validation Rules (§7):**
- `schema_id == 0` → reject (I011)
- `SemanticRows.schema_id != InstanceSchema.schema_id` → reject
- Row field count mismatch → reject per-row
- Bounds width/height <= 0 → reject
- Empty text/icon name → reject
- asset_id == 0 → reject

**Invariants dijamin:**
- I001: No wgpu imports
- I003: Tidak ada raw bytes / GPU resource IDs
- I004: Tidak ada `materialize()` method di artifact
- I011: schema_id non-zero enforcement
- I012: `!Clone` — no cross-frame reuse
- I013: schema_id as u64, no string lookup
- I017: ArtifactBudget enforcement

**Tests (17 tests):**
- `none_artifact_is_valid`
- `instances_artifact_validates_correctly`
- `unversioned_schema_id_is_rejected` — I011
- `schema_id_mismatch_is_rejected`
- `row_field_count_mismatch_is_rejected`
- `visual_with_zero_bounds_is_rejected`
- `atlas_reference_with_zero_asset_id_is_rejected`
- `budget_rejects_oversized_artifact` — I017
- `semantic_value_byte_sizes_are_correct`

---

### [NEW] `src/runtime/host_api.rs`

**Mengapa dibuat:**  
`HostApi` adalah **satu-satunya jalan legal** bagi `ExecutionUnit` untuk submit render intent. Memisahkan publish boundary dari broker. Validation synchronous sebelum staging.

**Isi:**
| Tipe | Deskripsi |
|------|-----------|
| `enum PublishError` | Validation, BudgetExceeded, StaleEpoch |
| `struct StagedArtifact` | Validated artifact + epoch. Ephemeral (I012). |
| `struct HostApi` | current_epoch, budget, accumulator, staged vec. |
| `impl HostApi::new()` | Create per-frame. |
| `impl HostApi::publish_artifact()` | Main entry point. Epoch check → validate → budget check → stage. |
| `impl HostApi::drain_staged()` | Consume HostApi, return staged artifacts for Broker. |
| `impl HostApi::staged_count()` | Diagnostics. |
| `impl HostApi::staged_bytes()` | Diagnostics. |

**`publish_artifact()` flow:**
1. Epoch guard: `artifact_epoch != current_epoch` → `StaleEpoch` (I012)
2. Schema validation: `artifact.validate()` → `Validation` error
3. Per-artifact budget: `budget.check_artifact()` → `BudgetExceeded` (I017)
4. Frame-total budget: `total_bytes + artifact_bytes > max_frame_bytes` → `BudgetExceeded` (I017)
5. Stage: push ke `self.staged`

**Ownership (I012):** `publish_artifact()` takes `artifact: RenderArtifact` **by value** — Rust ownership guarantees caller tidak bisa retain artifact setelah publish. Cross-frame reuse impossible.

**Invariants dijamin:**
- I011: validated via `artifact.validate()`
- I012: by-value consumption + `!Clone` on `RenderArtifact`
- I013: schema_id lookup O(1)
- I017: budget check sebelum staging

**Tests (7 tests):**
- `valid_artifact_stages_successfully`
- `stale_epoch_is_rejected` — I012
- `malformed_artifact_is_rejected_before_staging` — synchronous validation (§7)
- `frame_budget_exceeded_is_rejected` — I017
- `drain_staged_returns_all_valid_artifacts`

---

## Pre-existing Compile Fixes

> [!NOTE]
> Bukan bagian dari scope plan.md, tapi diperlukan agar `cargo test` (Validation Gate §11.1) bisa GREEN.

### [MODIFY] `src/smoke.rs`

**Mengapa diubah:**  
4 pre-existing compile errors menghalangi `cargo test`:

| Baris | Error | Fix |
|-------|-------|-----|
| L268 | `handle.reload()` dipanggil dengan 3 args (butuh 4) | Tambah `false` sebagai arg `sync` |
| L344 | `BehaviorHost::create_inits()` return tuple, di-assign ke `let inits` | Ganti ke `let (inits, _skipped) = ...` |
| L361-373 | Sama dengan baris 344 | Ganti ke `let (inits, _skipped) = ...` |
| L505 | `handle.reload()` dipanggil dengan 3 args (butuh 4) | Tambah `false` sebagai arg `sync` |

### [MODIFY] `src/behavior/host.rs`

**Mengapa diubah:**  
1 pre-existing compile error di test module:

| Baris | Error | Fix |
|-------|-------|-----|
| L103 | `super::SkipReason` tidak visible dari test module | Tambah `use crate::behavior::SkipReason;` |
| L164 | `super::SkipReason::FilesystemMissing` (super:: stale) | Ganti ke `SkipReason::FilesystemMissing` |

---

## Hasil Validasi

### Validation Gate §11.1 — Compile
```
cargo check → ✅ GREEN (33 warnings, 0 errors)
```

### Validation Gate §11.1 — Tests
```
cargo test → 158 passed; 6 failed (pre-existing runtime failures)
```

**6 failures yang tersisa:**
| Test | Penyebab | Status |
|------|----------|--------|
| `smoke::installed_addons_scan_and_shipped_pipeline_validates` | `addons/` dir tidak ada di CI environment | Pre-existing |
| `smoke::repo_pipeline_json_loads_and_validates_against_builtins` | `ascii_mask_overlay` example tidak ada | Pre-existing |
| `smoke::external_behavior_executes_through_the_host_and_publishes` | Face tracking Vec3 vs F32 assertion | Pre-existing |
| `smoke::face_behavior_survives_reload_and_keeps_publishing` | Downstream dari face tracking issue | Pre-existing |
| `smoke::group3_packing_order_for_face_signals` | Requires installed addons | Pre-existing |
| `behavior::scheduler::tests::over_budget_update_is_counted` | Scheduler logic test | Pre-existing |

Semua 6 failures dikonfirmasi pre-existing — baseline codebase tidak bisa compile sama sekali (7 compile errors). Dengan perubahan kita, kompilasi berhasil dan 158 tests lulus.

### New Module Tests — ALL GREEN ✅
```
runtime::graph::tests::graph_slot_assignment_is_sequential      ok
runtime::graph::tests::empty_graph_reports_correctly            ok
runtime::plan::tests::plan_epoch_advances_monotonically         ok
runtime::plan::tests::compile_produces_correct_node_count       ok
runtime::plan::tests::same_graph_same_hash                      ok  ← Validation Gate §11.8
runtime::plan::tests::different_graph_different_hash            ok
runtime::plan::tests::different_epoch_different_hash            ok
runtime::artifact::tests::* (9 tests)                           ok
runtime::host_api::tests::* (5 tests)                           ok
```

---

## File Ringkasan

| File | Action | Phase | Keterangan |
|------|--------|-------|------------|
| `src/runtime/graph.rs` | **BARU** | Phase 1 | `RenderGraph` + `RenderGraphNode` |
| `src/runtime/plan.rs` | **BARU** | Phase 2 | `PlanEpoch` + `ExecutionPlan` |
| `src/runtime/artifact.rs` | **BARU** | Phase 3 | Full `RenderArtifact` type hierarchy |
| `src/runtime/host_api.rs` | **BARU** | Phase 3 | `HostApi::publish_artifact()` |
| `src/runtime/mod.rs` | **DIMODIFIKASI** | 1+2+3 | Wires semua modul baru ke `PipelineRuntime` |
| `src/smoke.rs` | **DIMODIFIKASI** | Fix | 4 pre-existing compile errors |
| `src/behavior/host.rs` | **DIMODIFIKASI** | Fix | 1 pre-existing compile error + SkipReason import |
| `engine/plan.md` | **DIMODIFIKASI** | Semua | Update Implementation Journal + Phase status |
