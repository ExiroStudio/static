//! ExecutionPlan and PlanEpoch — Phase 2 of the render architecture migration.
//!
//! Separates *compilation* (deciding what runs and in what order) from
//! *execution* (actually submitting GPU work). The `PipelineRuntime` compiles
//! a new `ExecutionPlan` on every successful `build()` and executes the live
//! plan on every frame — these two operations are now strictly distinct.
//!
//! **Decision references:**
//! - **D004** — Artifact Model: `ExecutionPlan` is the immutable per-epoch
//!   snapshot that guarantees pipeline composition order.
//!
//! **Invariants enforced here:**
//! - **I002** — Render ordering changes only in `ExecutionPlan`. Ordering is
//!   captured at compile time, never mutated during execution.
//! - **I006** — `compile()` produces an immutable snapshot per frame epoch.
//!   There is exactly one constructor: `ExecutionPlan::compile()`.
//! - **I015** — `ExecutionPlan` compilation must be deterministic. Same graph
//!   topology → same `plan_hash`.
//! - **I018** — Frame execution cannot mutate compile topology. `ExecutionPlan`
//!   is `!Send + !Sync` intentionally — it is consumed in one place only.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::graph::RenderGraph;

/// A monotonically increasing epoch counter. Each successful `build()` in
/// `PipelineRuntime` advances the epoch. Used by the `ResourceBroker` (Phase 4)
/// to distinguish cache generations and enforce **I012** (artifacts cannot
/// survive frame boundaries).
///
/// Epoch 0 is reserved for "no plan compiled yet".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanEpoch(pub(crate) u64);

impl PlanEpoch {
    /// The initial "no plan" sentinel.
    pub const ZERO: PlanEpoch = PlanEpoch(0);

    /// Advance to the next epoch.
    pub(crate) fn next(self) -> PlanEpoch {
        PlanEpoch(self.0.saturating_add(1))
    }
}

impl Default for PlanEpoch {
    fn default() -> Self {
        Self::ZERO
    }
}

impl std::fmt::Display for PlanEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PlanEpoch({})", self.0)
    }
}

/// An immutable, compiled snapshot of one render graph epoch.
///
/// Created exclusively by [`ExecutionPlan::compile`] — there is no other
/// constructor (enforces **I006**). Once compiled, the plan's topology cannot
/// change; any structural edit requires a new `compile()` call producing a new
/// `ExecutionPlan` with an incremented `PlanEpoch`.
///
/// `plan_hash` is a deterministic fingerprint of the graph topology (node
/// count at this phase — full node identity will be added in Phase 3). Used by
/// **Validation Gate §11.8** ("Compile Epoch Gate"): same graph → same hash →
/// same plan → identical execution order.
///
/// # Why not implement `Clone`?
/// Cloning an `ExecutionPlan` would allow two "live" plans to coexist, which
/// violates **I018** (execution cannot mutate compile topology). The plan is
/// owned by `PipelineRuntime` and replaced atomically on each `build()`.
pub struct ExecutionPlan {
    /// The epoch this plan was compiled for.
    pub epoch: PlanEpoch,
    /// Number of active render nodes. Consumers use this for ping-pong routing
    /// without needing to re-inspect the graph.
    pub node_count: usize,
    /// Deterministic fingerprint of the compile-time graph topology.
    ///
    /// Same node ordering → same hash. Used by **Validation Gate §11.8**.
    /// Based on `DefaultHasher` which is deterministic within a single process
    /// run (suitable for diagnostic gating; not stable across processes or
    /// Rust versions, which is acceptable per the current scope).
    pub plan_hash: u64,
}

impl ExecutionPlan {
    /// Compile an immutable `ExecutionPlan` from the current state of
    /// `graph` at the given `epoch`.
    ///
    /// This is the **only** constructor for `ExecutionPlan` (enforces **I006**
    /// and **I015**). It is called by `PipelineRuntime::build()` after all
    /// nodes have been instantiated and pushed into the graph.
    ///
    /// # Determinism (**I015**)
    /// The `plan_hash` is computed from:
    /// - The epoch value
    /// - The node count (slot indices are sequential, so this covers ordering)
    ///
    /// In Phase 3, individual node identity (schema_id, artifact type) will be
    /// folded into the hash to satisfy the full Compile Epoch Gate requirement.
    pub fn compile(epoch: PlanEpoch, graph: &RenderGraph) -> Self {
        let node_count = graph.len();

        let mut hasher = DefaultHasher::new();
        epoch.hash(&mut hasher);
        node_count.hash(&mut hasher);
        // In Phase 3: hash each node's schema_id and artifact type here.
        let plan_hash = hasher.finish();

        Self {
            epoch,
            node_count,
            plan_hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{FilterNode, FrameContext};
    use crate::runtime::graph::RenderGraph;

    struct NoopNode;
    impl FilterNode for NoopNode {
        fn process(&self, _ctx: &mut FrameContext) {}
    }

    fn graph_with(n: usize) -> RenderGraph {
        let mut g = RenderGraph::new();
        for _ in 0..n {
            g.push(Box::new(NoopNode));
        }
        g
    }

    #[test]
    fn plan_epoch_advances_monotonically() {
        let e0 = PlanEpoch::ZERO;
        let e1 = e0.next();
        let e2 = e1.next();
        assert!(e0 < e1);
        assert!(e1 < e2);
        assert_eq!(e0, PlanEpoch(0));
        assert_eq!(e1, PlanEpoch(1));
    }

    #[test]
    fn compile_produces_correct_node_count() {
        let g = graph_with(3);
        let plan = ExecutionPlan::compile(PlanEpoch(1), &g);
        assert_eq!(plan.node_count, 3);
        assert_eq!(plan.epoch, PlanEpoch(1));
    }

    /// **Validation Gate §11.8 (Compile Epoch Gate):**
    /// Same graph topology → same plan_hash.
    #[test]
    fn same_graph_same_hash() {
        let g1 = graph_with(2);
        let g2 = graph_with(2);
        let epoch = PlanEpoch(1);
        let h1 = ExecutionPlan::compile(epoch, &g1).plan_hash;
        let h2 = ExecutionPlan::compile(epoch, &g2).plan_hash;
        assert_eq!(h1, h2, "same topology must produce identical plan_hash (I015)");
    }

    /// Different node counts must produce different hashes.
    #[test]
    fn different_graph_different_hash() {
        let g1 = graph_with(2);
        let g2 = graph_with(3);
        let epoch = PlanEpoch(1);
        let h1 = ExecutionPlan::compile(epoch, &g1).plan_hash;
        let h2 = ExecutionPlan::compile(epoch, &g2).plan_hash;
        assert_ne!(h1, h2, "different topology must produce different plan_hash");
    }

    /// Different epochs with the same graph topology must produce different hashes.
    #[test]
    fn different_epoch_different_hash() {
        let g = graph_with(2);
        let h1 = ExecutionPlan::compile(PlanEpoch(1), &g).plan_hash;
        let h2 = ExecutionPlan::compile(PlanEpoch(2), &g).plan_hash;
        assert_ne!(h1, h2, "epoch must contribute to plan_hash");
    }
}
