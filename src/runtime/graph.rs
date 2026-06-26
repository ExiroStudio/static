//! RenderGraph skeleton — Phase 1 of the render architecture migration.
//!
//! Introduces a graph-local ordering layer *above* the raw `FilterNode` list
//! without changing any execution semantics. The `PipelineRuntime` continues
//! to drive `FilterNode::process()` in sequential order; this module adds the
//! type-level infrastructure needed for Phase 2 (`ExecutionPlan`) and Phase 3
//! (`RenderArtifact`).
//!
//! **Decision references:**
//! - **D002** — Render Graph Migration: this is the skeleton; full migration
//!   deferred until Artifact ABI + ResourceBroker exist.
//! - **D006** — Architecture First Rename: `FilterNode` persists internally.
//!   The public trait rename (`RenderNode`) happens in Phase 3 only.
//!
//! **Invariants enforced here:**
//! - **I002** — Render ordering changes only in `ExecutionPlan` (Phase 2).
//!   `RenderGraph` is the authoritative ordered list; it does not execute.
//! - **I007** — Nodes remain graph-local. `RenderGraphNode` never escapes this
//!   module boundary into the addon or behavior layer.

use crate::runtime::FilterNode;

/// A single node entry inside the [`RenderGraph`].
///
/// Wraps a live [`FilterNode`] with its graph-local identity. The wrapper is
/// intentionally opaque — callers use `RenderGraph` methods rather than
/// reaching into the node directly. This preserves the boundary required by
/// **I007** (nodes are graph-local).
///
/// # Note on naming
/// This type will be renamed / exposed as `RenderNode` in Phase 3 (see **D006**
/// and §3 of `plan.md`). For now it is graph-internal only.
pub(crate) struct RenderGraphNode {
    /// The unique, stable addon instance ID. Used for stable ResourceBroker mapping.
    pub(crate) instance_id: String,
    /// Stable graph-local identity for diagnostics and future `ExecutionPlan`
    /// slot assignment. Assigned once at `push` time; never reused within a
    /// single graph instance.
    pub(crate) slot: usize,
    /// The live filter implementation. Kept as `Box<dyn FilterNode>` to match
    /// the existing `PipelineRuntime` ownership model. **D006**: the internal
    /// name stays `FilterNode` until Phase 3.
    pub(crate) node: Box<dyn FilterNode>,
}

/// An ordered collection of [`RenderGraphNode`]s representing a single
/// compiled render graph.
///
/// `RenderGraph` is the authoritative source of node ordering (**I002**). The
/// `PipelineRuntime` owns one of these at a time; a new graph replaces the old
/// one atomically on each successful `build()`. This ensures ordering decisions
/// are never mutated mid-frame (**I018**).
///
/// Currently the graph is strictly linear (Node 0 → Node 1 → … → Sink), which
/// mirrors the existing ping-pong pipeline. Graph branching is **explicitly
/// forbidden** until Phase 3 is complete (§15 of `plan.md`).
pub struct RenderGraph {
    nodes: Vec<RenderGraphNode>,
}

impl RenderGraph {
    /// Create an empty graph. Nodes are appended via [`push`](Self::push).
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    /// Append a live filter node to the graph and assign it the next slot index.
    ///
    /// Slot indices are monotonically increasing within a graph instance and
    /// are used by [`ExecutionPlan`](super::plan::ExecutionPlan) (Phase 2) for
    /// ping-pong routing. They are **not** stable across graph rebuilds.
    pub fn push(&mut self, instance_id: String, node: Box<dyn FilterNode>) {
        let slot = self.nodes.len();
        self.nodes.push(RenderGraphNode { instance_id, slot, node });
    }

    /// The number of nodes in the graph.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns `true` if the graph contains no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Immutable ordered view of the graph nodes.
    ///
    /// Intentionally returns `&[RenderGraphNode]` rather than an iterator of
    /// bare `FilterNode`s to preserve the graph-local wrapper (**I007**). The
    /// runtime accesses the inner node through `.node` only within its own
    /// execution loop.
    pub(crate) fn nodes(&self) -> &[RenderGraphNode] {
        &self.nodes
    }

    /// Mutable ordered view, needed by `PipelineRuntime::render()` to call
    /// `FilterNode::prepare()` which takes `&mut self`.
    pub(crate) fn nodes_mut(&mut self) -> &mut [RenderGraphNode] {
        &mut self.nodes
    }
}

impl Default for RenderGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::FrameContext;
    use wgpu::Queue;
    use crate::signal::SignalSnapshot;

    /// Minimal stub to satisfy `FilterNode` in tests (no GPU needed).
    struct NoopNode;
    impl FilterNode for NoopNode {
        fn process(&self, _ctx: &mut FrameContext) {}
    }

    #[test]
    fn graph_slot_assignment_is_sequential() {
        let mut g = RenderGraph::new();
        g.push("a".into(), Box::new(NoopNode));
        g.push("b".into(), Box::new(NoopNode));
        g.push("c".into(), Box::new(NoopNode));

        assert_eq!(g.len(), 3);
        let slots: Vec<usize> = g.nodes().iter().map(|n| n.slot).collect();
        assert_eq!(slots, vec![0, 1, 2], "slots must be 0-indexed sequential");
    }

    #[test]
    fn empty_graph_reports_correctly() {
        let g = RenderGraph::new();
        assert!(g.is_empty());
        assert_eq!(g.len(), 0);
    }
}
