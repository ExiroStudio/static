# AGENTS.md — Architectural Constitution

This document is the architectural constitution of the `ascii-realtime` repository. Its audience is both human contributors and AI coding assistants. It explicitly defines the boundaries, responsibilities, and forbidden operations within the codebase.

## 1. Core Philosophy

*   **Execution computes.** (External addons perform math, layout, and logic)
*   **Artifacts describe.** (Semantics are passed as pure-data payloads)
*   **Broker materializes.** (Semantics are translated to byte-aligned GPU memory)
*   **Render executes.** (The graph proxy nodes record commands)
*   **GPU renders.** (Hardware execution)

Every layer has exactly one responsibility. Do not blur these boundaries.

## 2. Ownership Rules

The **Engine** strictly owns:
*   `wgpu::Device` and `wgpu::Queue`
*   `wgpu::Buffer` and `wgpu::Texture`
*   `wgpu::RenderPipeline` and `wgpu::BindGroup`
*   Resource lifetime and eviction
*   Render ordering and Graph topology

The **ExecutionUnits** (Addons) own ONLY:
*   Semantic computation and layout logic

## 3. Forbidden Rules

These rules are non-negotiable.

*   ❌ `ExecutionUnit` must never import `wgpu`.
*   ❌ `packing.rs` must never import `wgpu`.
*   ❌ `RenderArtifact` must never store GPU handles.
*   ❌ `RenderArtifact` must never contain BindGroup IDs.
*   ❌ `RenderArtifact` must never contain Queue references.
*   ❌ `RenderArtifact` must never contain Device references.
*   ❌ `RenderArtifact` must never own physical resources.
*   ❌ `ResourceBroker` must never expose wgpu objects to addons.
*   ❌ `PrepareContext` is the ONLY place allowed to invoke materialization.
*   ❌ `RenderNode::execute()` must never allocate GPU resources.
*   ❌ `execute()` must never call `create_buffer()`.
*   ❌ `execute()` must never resize GPU buffers.
*   ❌ `execute()` must never upload resources.
*   ❌ `ExecutionPlan` must never mutate during a frame.
*   ❌ `ExecutionUnit` must never know GPU memory layout.
*   ❌ `ExecutionUnit` must never calculate std140/std430 alignment.
*   ❌ `ExecutionUnit` must never care about padding.
*   ❌ Addons must never bypass `ResourceBroker`.
*   ❌ Addons must never directly access `Queue`.
*   ❌ Addons must never directly access `Device`.
*   ❌ Addons must never create `BindGroup`s.
*   ❌ Addons must never create `RenderPipeline`s.
*   ❌ Materialization must never happen inside addons.
*   ❌ Global render state is forbidden.

## 4. Required Rules

What every subsystem MUST do:

*   **ExecutionUnit:**
    *   ✔ Compute semantics.
    *   ✔ Publish `RenderArtifact`.
*   **Broker:**
    *   ✔ Translate semantics into physical buffers.
    *   ✔ Allocate GPU memory via `Allocator`.
    *   ✔ Upload data via `Uploader`.
    *   ✔ Cache resources and layouts.
    *   ✔ Track metrics.
*   **RenderNode:**
    *   ✔ Consume physical resources provided by the Broker.
    *   ✔ Record draw commands.
*   **RenderRuntime:**
    *   ✔ Preserve graph ordering.
    *   ✔ Execute immutable `ExecutionPlan`.

## 5. Architectural Invariants

*   **I001:** ExecutionUnit never owns GPU. Execution and rendering are separate domains.
*   **I002/I018:** Graph ordering must remain deterministic. Frame execution cannot mutate compile topology.
*   **I003:** Artifacts remain semantic.
*   **I004/I014:** Broker is the only GPU translation layer. Broker materializes; Broker never computes.
*   **I012:** Semantic artifacts are ephemeral. They belong to exactly one FrameEpoch.
*   **I016:** Physical resources are persistent. Resource ownership survives hot reload.
*   **I020:** Execution never talks to the GPU for allocations. Execution Phase must never allocate GPU resources. Allocation strictly belongs to the Prepare Phase.

## 6. Decision Principles

When adding new features, always ask:

1.  **Does this leak GPU ownership?**
    *   If yes: **STOP.**
2.  **Does this bypass ResourceBroker?**
    *   If yes: **STOP.**
3.  **Does this merge semantic and physical layers?**
    *   If yes: **STOP.**
4.  **Does this duplicate an existing runtime?**
    *   If yes: **STOP.**
5.  **Does this reduce long-term maintainability?**
    *   If yes: **STOP.**

## 7. AI Contributor Rules

Before generating code, AI assistants MUST:

*   Read `plan.md`.
*   Read `AGENTS.md`.
*   Follow existing architecture.
*   Never redesign the engine unless explicitly requested.
*   Never introduce compatibility wrappers.
*   Never bypass architectural boundaries.
*   Never optimize by violating ownership.
*   Prefer compile-time guarantees over runtime checks.
*   Prefer removing abstraction debt over preserving legacy APIs.
*   If your intended implementation conflicts with `AGENTS.md`, **stop and explain why** instead of generating code.

## 8. Documentation Rule

Whenever an architectural decision changes:

1.  Update `plan.md` first.
2.  Then update `README.md`.
3.  Then update `AGENTS.md`.

Never allow these three documents to diverge.
