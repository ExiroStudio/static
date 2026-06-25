# Render Architecture Ledger

## 0 Executive Summary

**Purpose:**
This document is the single source of truth for the `ascii-realtime` engine's rendering architecture. It supersedes all previous architectural experiments, proposals, and migration plans. It is an append-first ledger: architecture evolves, but history is preserved.

**Vision:**
Evolve the engine's rendering system from a rigid, fullscreen-only filter pipeline into a capability-driven render graph capable of supporting complex, data-driven external render workloads (e.g., MSDF text, particles, 3D overlays) while strictly isolating GPU ownership from external addon logic.

**Final Direction:**
A single unified Execution Platform runs all external logic (Behaviors and Render Generators). These ExecutionUnits publish purely semantic `RenderArtifact`s to a `ResourceBroker`. The `RenderRuntime` then compiles an immutable `ExecutionPlan` per epoch and uses proxy `RenderNode`s to consume the artifacts, commanding the GPU in perfect sequential order without exposing any `wgpu` resources to the addons.

**Non Goals:**
*   Adding a full ECS (Entity Component System) or Scene Graph.
*   Preserving legacy `FilterNode` compatibility wrappers.
*   Supporting multi-window output or split-graph branching.
*   Allowing external addons to write custom `wgpu` shader passes dynamically.

---

## 1 Current Inventory

*   **Current Runtime:** `PipelineRuntime` executes a rigid, sequential ping-pong array.
*   **Current Traits:** `FilterNode` (takes `FrameContext`, expects fullscreen processing).
*   **Current Ownership:** Engine owns `wgpu::Device`, but external addons are starting to demand direct buffer access (e.g., MSDF).
*   **Current Graph:** Implicitly linear. Node 0 → Node 1 → Node 2.
*   **Current Constraints:** Cannot support instanced rendering, dynamic geometry, or CPU layout bounds without massive hacks.

---

## 2 Accepted Decisions

| ID | Title | Status | Reason | Impact |
| :--- | :--- | :--- | :--- | :--- |
| **D001** | Single Execution Platform | **Accepted** | BehaviorRuntime and RenderRuntime must not duplicate process/WASM lifecycles. Execution Platform handles all `.so`/WASM loading. | Unifies supervisor, sandbox, and IPC. |
| **D002** | Render Graph Migration | **Accepted** | Temporary wrappers (`FilterCompat`) create massive long-term tech debt. FilterNode removal remains target state, deferred until RenderArtifact ABI, ResourceBroker, and Execution Platform integration exist. | Intentional compile break across all builtins. Clean migration. Avoids multiple public trait mutations. |
| **D003** | Engine Owns GPU | **Accepted** | Allowing addons to allocate buffers directly causes resource leaks, OOMs, and state desyncs. | Addons use Broker; zero direct `wgpu` access. |
| **D004** | Artifact Model | **Accepted** | ExecutionUnit logic must not ruin graph order. Artifacts stay graph-local to guarantee pipeline composition (e.g., Overlay → CRT → Bloom). | Purity of execution order. |
| **D005** | Semantic Payload | **Accepted** | `Vec<u8>` leaks GPU memory padding/alignment into addons, causing ABI breakage if engine GPU packing changes. | Addons describe *what*; Broker handles *how* (packing). |
| **D006** | Architecture First Rename | **Accepted** | Public API rename only occurs after final execution boundaries exist. Avoid trait mutation chains. | Phase 1 focuses purely on graph skeleton. `FilterNode` persists internally. |

---

## 3 Rejected Decisions

| Proposal | Why Rejected | Future Reopen Conditions |
| :--- | :--- | :--- |
| **Builtin MSDF** | Engine core should not own specific feature implementations. Violates addon ecosystem goal. | N/A (MSDF must be external). |
| **Global Render Data** | `HostApi::update_render_data()` collapses graph-local ordering into global state, breaking composability. | Only if the pipeline becomes purely a 3D scene graph without 2D passes. |
| **FilterCompat Node** | Wraps old `FilterNode`s inside new `RenderNode`s. Adds abstraction layers that must be maintained forever. | N/A (One-time migration preferred). |
| **GPU Exposure via ABI** | Exposing `wgpu::Queue` or `BindGroup` to WASM/native addons destroys security and multi-threading capabilities. | Reopen if WebGPU natively supports trusted WASM binding sharing in the future. |
| **Runtime Duplication** | Allowing `RenderRuntime` to spawn processes to run render logic creates two Supervisors. | N/A. |

---

## 4 Ownership Matrix

| Subsystem | Owner | Details |
| :--- | :--- | :--- |
| **Execution** | `Execution Platform` | Process/WASM spawning, IPC, crash recovery. |
| **Layout** | `ExecutionUnit` (Addon) | Parses text, generates bounding boxes, kerning logic. |
| **Artifacts** | `RenderArtifact` | Semantic, pure-data description of intent. |
| **Render Ordering** | `RenderGraph` | Creates the `ExecutionPlan` (ping-pong slots). |
| **GPU Allocation** | `ResourceBroker` | Texture atlas, buffer pooling, vertex alignment/packing. |
| **Draw Submission** | `RenderRuntime` | Runs the `RenderNode::execute()` loop. |

---

## 5 Architecture Invariants

*   **I001:** ExecutionUnit never owns GPU.
*   **I002:** Render ordering changes only in ExecutionPlan.
*   **I003:** Artifacts remain semantic.
*   **I004:** Broker performs materialization.
*   **I005:** ExecutionPlatform never records draw commands.
*   **I006:** `compile()` immutable per frame.
*   **I007:** Nodes remain graph-local.
*   **I008:** Resource ownership survives hot reload.
*   **I009:** Decision history cannot be rewritten.
*   **I010:** Architecture status independent from implementation.
*   **I011:** Artifact must remain statically materializable. No runtime schema discovery.
*   **I012:** Artifact belongs to exactly one FrameEpoch. Artifact cannot survive frame boundaries. Cross-frame artifact reuse forbidden.
*   **I013:** Artifact schema resolution must be amortized. Per-frame schema reconstruction forbidden.
*   **I014:** Broker materializes. Broker never computes.
*   **I015:** ExecutionPlan compilation must be deterministic.

Implementation violating an invariant must stop.

---

## 6 Final Runtime Blueprint

```text
[Execution Platform]
        │ (Spawns native/WASM logic)
        ▼
[ExecutionUnit (External Addon)]
        │ (Computes layout, positions, semantics)
        ▼
[RenderArtifact (Semantic Payload)]
        │ (Passed via HostApi)
        ▼
[PrepareContext]
        │ (Staged for frame execution)
        ▼
[ResourceBroker]
        │ (Materializes physical GPU buffers, applies padding)
        ▼
[RenderNode (Proxy instance inside RenderGraph)]
        │ (Reads physical buffers, records commands)
        ▼
[ExecutionPlan]
        │ (Maintains exact ping-pong graph routing)
        ▼
[GPU]
```

---

## 7 Artifact Specification

The `RenderArtifact` ABI describes semantic intent without any memory assumptions. No `Vec<u8>`, no raw bytes, no resource IDs.

### Artifact Validation Rules
*   **Rows count:** must strictly match schema bounds.
*   **Field order:** is immutable.
*   **Missing field:** reject immediately.
*   **Extra field:** reject immediately.
*   **Schema:** must be versioned.
*   **Validation:** happens synchronously at `HostApi::publish_artifact()`. Malformed artifacts are rejected before reaching the broker.

### Artifact Lifecycle
*   **Artifacts:** Ephemeral, frame-scoped.
*   **Materialized Resources:** Cacheable, reusable, broker-owned.
*   **Lifecycle Flow:** `Generate` → `Publish` → `Stage` → `Materialize` → `Consume` → `Drop Artifact` → `Retain Resource`
*   *Reason:* MSDF atlas and future instancing should survive frame boundaries, while the layout data drops.

```rust
pub enum RenderArtifact {
    None,
    Instances {
        schema: InstanceSchema,
        rows: SemanticRows, // Immutable, amortized schema parsing
    },
    Geometry {
        topology: PrimitiveTopology,
        vertices: SemanticRows,
    },
    Overlay {
        content: OverlayContent,
        bounds: [f32; 4],
    },
    AtlasReference {
        asset_name: String,
        region: [f32; 4],
    },
    Custom {
        schema_id: u64,
        rows: SemanticRows,
    }
}

pub enum OverlayContent {
    Text {
        content: String,
        mode: TextMode,
    },
    Icon(String),
    AtlasRegion {
        asset: String,
        region: [f32; 4],
    },
    Custom {
        schema_id: u64,
    }
}

pub enum TextMode {
    Plain,
    Rich,
    Glyph,
}

pub struct SemanticRows {
    pub schema_id: u64,
    pub rows: std::sync::Arc<[SemanticRow]>,
}

pub struct InstanceSchema {
    pub kind: String, // e.g., "glyph", "particle"
    pub fields: Vec<SemanticField>,
}

pub enum SemanticField {
    Position2,
    Position3,
    ColorRgba,
    UvQuad,
    CustomFloat(String),
}

/// A single row of data (e.g., one glyph instance).
/// The broker flattens this into a tightly packed `[u8]` buffer based on `wgpu` limits.
pub struct SemanticRow {
    pub values: Vec<SemanticValue>,
}

pub enum SemanticValue {
    Float(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
}
```

### Internal Materialization Policy

**Rules:**
The Broker MAY:
*   Normalize rows
*   Transform AoS (Array of Structs) → SoA (Struct of Arrays)
*   Repack memory
*   Batch uploads

The Broker MUST NOT:
*   Mutate semantic meaning
*   Reorder visible output

*Reason:* Provides optimization freedom for large-scale particles, MSDF batching, and future 3D overlays without changing the ABI.

---

## 8 ResourceBroker

**Responsibilities:**
*   Translates semantic `RenderArtifact`s into physical GPU `Buffer`s.
*   Calculates `std140`/`std430` padding, alignment, and stride internally.
*   Pools buffer allocations to prevent per-frame GPU memory thrashing.

**Lifecycle:**
State progression: `Allocated` → `Warm` → `Active` → `Stale` → `Grace Window` → `Collected`

*   **Warm:** Resource exists, materialized, not consumed yet. Used for hot reload, atlas preload, and shader recompilation.
*   **Eviction:** An epoch change does not instantly destroy resources.
*   **Grace Window Rules:** A resource exits Grace Window and is `Collected` if:
    *   `frames_unused > threshold` OR
    *   `memory_pressure > limit`
*   **Hot Reload:** Absolutely safe; resources persist through the Grace Window.
*   **Persistent Assets:** Atlas and static textures must explicitly opt in to persistence to prevent immortal cache growth.

**Broker Authority:**
*   **Broker MAY:** pack, align, batch, cache, normalize memory.
*   **Broker MUST NOT:** generate geometry, infer semantic layout, calculate transforms, synthesize artifacts, mutate semantic meaning.
*   **Broker never owns:** graph order, execution timing, addon lifecycle.
*   *Reason:* Prevent Broker becoming a hidden scheduler or executing render logic.

---

## 9 Migration Timeline

*   **Phase 1: Clean Break**
    *   **Status:** Accepted
    *   **Implementation:** Reverted
    *   **Validation:** Not Started
    *   **Reason:** Prevent architecture drift. Prototype was completed before architecture freeze.
    *   *Details:* Introduce `RenderGraph` skeleton. Keep `FilterNode` internally. No public API rename. No execution change. No migration yet. Avoid premature trait mutation chains (see **D006**).

*   **Phase 2: Execution Plan**
    *   **Status:** Accepted
    *   **Implementation:** Reverted
    *   **Validation:** Not Started
    *   **Reason:** Prevent architecture drift.
    *   *Details:* Introduce `ExecutionPlan` and `PlanEpoch`. Separate compilation from execution.

*   **Phase 3: Semantic Artifact ABI**
    *   **Status:** Planned
    *   **Implementation:** Not Started
    *   **Validation:** Not Started
    *   *Details:* Implement `RenderArtifact` and `SemanticRow` in the shared addon library. Wire `HostApi::publish_artifact()`. Perform `RenderNode` public trait rename here, once boundaries exist.

*   **Phase 4: Resource Broker**
    *   **Status:** Planned
    *   **Implementation:** Not Started
    *   **Validation:** Not Started
    *   *Details:* Implement `ResourceBroker` memory management. Implement the semantic-to-physical packing algorithm.

*   **Phase 5: MSDF Implementation**
    *   **Status:** Planned
    *   **Implementation:** Not Started
    *   **Validation:** Not Started
    *   *Details:* Build the generic `InstancedOverlayNode` proxy. Implement MSDF entirely as a CPU-based `ExecutionUnit`.

**Rollback Strategy:** Atomic `git reset --hard` to phase-aligned commits.

---

## 10 Risk Ledger

| Risk ID | Severity | Mitigation | Status |
| :--- | :--- | :--- | :--- |
| **R001** | High | **IPC Bottleneck:** Serializing large `SemanticRow` vectors over WASM/Native boundary might kill FPS. | Use zero-copy shared memory blocks if standard serialization becomes a bottleneck. | Active |
| **R002** | Medium | **Synchronization:** `ExecutionUnit` layout ticking asynchronously from Render 60fps might cause screen tearing. | Broker must double-buffer artifacts or latch onto strict frame IDs. | Active |

---

## 11 Validation Gates

Each Phase must pass these gates before merging:
1.  **Compile:** `cargo check` and `cargo test` must be GREEN (no new failures).
2.  **Runtime:** Must boot without panic.
3.  **Memory:** Broker must not leak `wgpu::Buffer`s across epochs.
4.  **FPS:** Must maintain 60FPS on base webcam feed.
5.  **Ownership:** No `wgpu` types may exist in the `addons/msdf/` source code.
6.  **Determinism Gate:** Same artifact + same signals = identical output. Failure blocks merge.
    *   **Metric (`frame_hash`):** Captures draw count, broker allocations, and render output hash.
7.  **Artifact Purity Gate:** Same input → same artifact → same output.
    *   Artifacts must remain immutable after publish. Broker materialization cannot mutate artifact. Failure blocks merge.
    *   **Metrics:** `artifact_hash`, `frame_hash`, `allocation_count`.
8.  **Compile Epoch Gate:** Same graph → same compile → same execution_plan → same plan_hash.
    *   Mismatch = BLOCK.
    *   **Metrics:** `plan_hash`, `allocation_hash`, `frame_hash`.

---

## 12 Implementation Journal

**Architecture Status ≠ Implementation Status**
*   `Accepted` ≠ implemented
*   `Implemented` ≠ validated
*   `Validated` ≠ frozen
*   `Implemented` ≠ shipped
*   `Reverted` ≠ rejected

*Format: YYYY-MM-DD | Context | Decision | Consequence | Status*

*   **2026-06-25** | Initial RenderNode trait swap | D002 | Compile break resolved | **Reverted** (Implementation ahead of architecture)
*   **2026-06-25** | PlanEpoch and ExecutionPlan added | D004 | Node ordering separated from execution | **Reverted** (Implementation ahead of architecture)
*   **2026-06-25** | Finalized Artifact Model | D005 | Replaced `Vec<u8>` with `SemanticRow` | **Accepted**

---

## 13 Future Extensions

**MSDF**
*   Acceptance: Renders crisp text, follows graph order, survives hot-reload.
*   **Strict Entry Point:**
    ```text
    ExecutionUnit → RenderArtifact → ResourceBroker → Proxy RenderNode → GPU
    ```
    *   **Forbidden:** `addons/msdf/*` → `wgpu`
    *   **Forbidden:** `RenderRuntime` → load native runtime

**Particles**
*   Acceptance: Supports 10,000+ instances driven by `SemanticRow` outputs from a WASM physics behavior.

**3D Overlays**
*   Acceptance: Supports `SemanticField::Position3` gracefully mapping to a basic depth-tested proxy node.

---

## 14 plan.md Governance

*   Architecture can evolve. Architecture changes REQUIRE:
    *   Decision ID
    *   Reason
    *   Cost
    *   Rollback
    *   Validation
*   **Missing fields → reject change.** Decision without cost analysis is invalid.
*   History cannot disappear.
*   Old decisions remain visible.
*   `Superseded` ≠ deleted.
*   `Rejected` ≠ removed.
*   Implementation files must explicitly reference **Decision IDs** in code comments or PR descriptions.

---

## 15 Freeze Scope

**Allowed:**
*   Artifact ABI
*   Broker internals
*   Validation

**Forbidden until Phase 3 complete:**
*   New render roles
*   New capability flags
*   MSDF optimization
*   New runtime
*   Graph branching
*   Multi-window

*Reason:* Stop endless architecture expansion.
