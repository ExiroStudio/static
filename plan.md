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
| **D002** | RenderNode Replaces FilterNode | **Accepted** | Temporary wrappers (`FilterCompat`) create massive long-term tech debt. | Intentional compile break across all builtins. Clean migration. |
| **D003** | Engine Owns GPU | **Accepted** | Allowing addons to allocate buffers directly causes resource leaks, OOMs, and state desyncs. | Addons use Broker; zero direct `wgpu` access. |
| **D004** | Artifact Model | **Accepted** | ExecutionUnit logic must not ruin graph order. Artifacts stay graph-local to guarantee pipeline composition (e.g., Overlay → CRT → Bloom). | Purity of execution order. |
| **D005** | Semantic Payload | **Accepted** | `Vec<u8>` leaks GPU memory padding/alignment into addons, causing ABI breakage if engine GPU packing changes. | Addons describe *what*; Broker handles *how* (packing). |

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

## 5 Final Runtime Blueprint

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

## 6 Artifact Specification

The `RenderArtifact` ABI describes semantic intent without any memory assumptions. No `Vec<u8>`, no raw bytes, no resource IDs.

```rust
pub enum RenderArtifact {
    None,
    Instances {
        schema: InstanceSchema,
        rows: Vec<SemanticRow>, // Semantic fields, NOT packed bytes
    },
    Geometry {
        topology: PrimitiveTopology,
        vertices: Vec<SemanticRow>,
    },
    Overlay {
        content: String,
        bounds: [f32; 4],
    },
    AtlasReference {
        asset_name: String,
        region: [f32; 4],
    },
    Custom {
        schema_name: String,
        data: serde_json::Value,
    }
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

---

## 7 ResourceBroker

**Responsibilities:**
*   Translates semantic `RenderArtifact`s into physical GPU `Buffer`s.
*   Calculates `std140`/`std430` padding, alignment, and stride internally.
*   Pools buffer allocations to prevent per-frame GPU memory thrashing.

**Lifecycle:**
*   Allocations are tied to the `PlanEpoch`.
*   **Growth:** Buffers dynamically resize if `rows.len()` exceeds current capacity.
*   **Eviction:** When the `RenderGraph::compile()` advances the `PlanEpoch`, the Broker sweeps and drops any buffer cache keys associated with the old epoch.

---

## 8 Migration Timeline

*   **Phase 1: Clean Break** (Status: **COMPLETED**)
    *   Renamed `FilterNode` → `RenderNode`.
    *   Migrated builtins (CRT, DotRenderer).
    *   Compile break resolved.
*   **Phase 2: Execution Plan** (Status: **COMPLETED**)
    *   Introduced `RenderGraph`, `ExecutionPlan`, and `PlanEpoch`.
    *   Separated compilation from execution.
*   **Phase 3: Semantic Artifact ABI** (Status: PENDING)
    *   Implement `RenderArtifact` and `SemanticRow` in the shared addon library.
    *   Wire `HostApi::publish_artifact()`.
*   **Phase 4: Resource Broker** (Status: PENDING)
    *   Implement `ResourceBroker` memory management.
    *   Implement the semantic-to-physical packing algorithm.
*   **Phase 5: MSDF Implementation** (Status: PENDING)
    *   Build the generic `InstancedOverlayNode` proxy.
    *   Implement MSDF entirely as a CPU-based `ExecutionUnit`.

**Rollback Strategy:** Atomic `git reset --hard` to phase-aligned commits.

---

## 9 Risk Ledger

| Risk ID | Severity | Mitigation | Status |
| :--- | :--- | :--- | :--- |
| **R001** | High | **IPC Bottleneck:** Serializing large `SemanticRow` vectors over WASM/Native boundary might kill FPS. | Use zero-copy shared memory blocks if standard serialization becomes a bottleneck. | Active |
| **R002** | Medium | **Synchronization:** `ExecutionUnit` layout ticking asynchronously from Render 60fps might cause screen tearing. | Broker must double-buffer artifacts or latch onto strict frame IDs. | Active |

---

## 10 Validation Gates

Each Phase must pass these gates before merging:
1.  **Compile:** `cargo check` and `cargo test` must be GREEN (no new failures).
2.  **Runtime:** Must boot without panic.
3.  **Memory:** Broker must not leak `wgpu::Buffer`s across epochs.
4.  **FPS:** Must maintain 60FPS on base webcam feed.
5.  **Ownership:** No `wgpu` types may exist in the `addons/msdf/` source code.

---

## 11 Implementation Journal

*   **2026-06-25** — Phase 1 Completed. Trait `FilterNode` replaced. (Decision Ref: D002)
*   **2026-06-25** — Phase 2 Completed. `ExecutionPlan` introduced. Ordering separated from execution.
*   **2026-06-25** — Artifact Model finalized to be purely semantic (No `Vec<u8>`). (Decision Ref: D005)

---

## 12 Future Extensions

**MSDF**
*   Acceptance: Renders crisp text, follows graph order, survives hot-reload.

**Particles**
*   Acceptance: Supports 10,000+ instances driven by `SemanticRow` outputs from a WASM physics behavior.

**3D Overlays**
*   Acceptance: Supports `SemanticField::Position3` gracefully mapping to a basic depth-tested proxy node.
