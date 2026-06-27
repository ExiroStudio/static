//! HostApi — the publish boundary between ExecutionUnit and ResourceBroker.
//!
//! This is the Phase 3 seam: the only legal path for an `ExecutionUnit` (addon
//! logic) to submit render intent to the engine. Artifacts are validated
//! synchronously here — malformed payloads never reach the broker.
//!
//! **Decision references:**
//! - **D003** — Engine Owns GPU: addons submit artifacts; the Broker allocates.
//! - **D005** — Semantic Payload: no `Vec<u8>`, no raw bytes cross this boundary.
//!
//! **Invariants enforced here:**
//! - **I011** — Artifact must remain statically materializable.
//! - **I012** — Artifact belongs to exactly one `FrameEpoch`. Enforced by
//!   `publish_artifact` consuming the `RenderArtifact` by value and binding it
//!   to the current frame's epoch. The caller cannot retain the artifact after
//!   publishing (Rust ownership — compile-time guarantee).
//! - **I013** — Schema resolution is amortized (no per-call string lookup).
//! - **I017** — Artifacts cannot force frame starvation. Budget is checked
//!   before any staging occurs. Overflow → drop + warn, never block.

use super::artifact::{ArtifactBudget, ArtifactValidationError, RenderArtifact};
use super::plan::PlanEpoch;

/// The error type returned by `HostApi::publish_artifact`.
///
/// All variants are non-fatal from the render thread's perspective: the bad
/// artifact is dropped and the frame continues (**I017**).
#[derive(Debug, Clone, PartialEq)]
pub enum PublishError {
    /// The artifact failed schema validation (§7 of `plan.md`).
    Validation(ArtifactValidationError),
    /// The artifact would exceed the per-artifact or per-frame byte budget (**I017**).
    BudgetExceeded(ArtifactValidationError),
    /// The artifact was published for an epoch that is no longer current.
    /// This can happen if an `ExecutionUnit` is lagging behind frame pacing.
    StaleEpoch {
        artifact_epoch: PlanEpoch,
        current_epoch: PlanEpoch,
    },
}

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishError::Validation(e) => write!(f, "artifact validation failed: {e}"),
            PublishError::BudgetExceeded(e) => write!(f, "artifact budget exceeded: {e}"),
            PublishError::StaleEpoch {
                artifact_epoch,
                current_epoch,
            } => write!(
                f,
                "artifact published for stale epoch {artifact_epoch} (current: {current_epoch})"
            ),
        }
    }
}

impl std::error::Error for PublishError {}

/// A staged (validated, budget-checked) artifact ready for the `ResourceBroker`.
///
/// Only `HostApi::publish_artifact` can construct this (no public fields).
/// The `ResourceBroker` consumes it to allocate physical GPU memory.
///
/// # Lifetime note
/// `StagedArtifact` is ephemeral — it exists only between `publish_artifact`
/// and the broker's `materialize` call within the same frame (**I012**).
pub struct StagedArtifact {
    pub(crate) instance_id: String,
    pub artifact: RenderArtifact,
    epoch: PlanEpoch,
}

impl StagedArtifact {
    /// Read the epoch this artifact was staged for.
    pub fn epoch(&self) -> PlanEpoch {
        self.epoch
    }
}

/// Per-frame accumulator state for the HostApi. Tracks the running byte total
/// against `ArtifactBudget::max_frame_bytes` (**I017**).
struct FrameAccumulator {
    total_bytes: usize,
    total_rows: usize,
}

/// The publish boundary between an `ExecutionUnit` and the `ResourceBroker`.
///
/// One `HostApi` instance exists per frame. After the frame ends, it is dropped
/// (enforcing **I012** — artifacts cannot survive frame boundaries).
///
/// # Usage
/// ```ignore
/// let mut api = HostApi::new(current_epoch, ArtifactBudget::default());
/// api.publish_artifact(instance_id, artifact)?;
/// // api is dropped at end of frame; staged artifacts passed to broker.
/// ```
pub struct HostApi {
    current_epoch: PlanEpoch,
    budget: ArtifactBudget,
    accumulator: FrameAccumulator,
    staged: Vec<StagedArtifact>,
    time: f32,
    dt: f32,
}

impl HostApi {
    /// Create a new `HostApi` for the given epoch and budget.
    ///
    /// Called once per frame by `PipelineRuntime::render()` (Phase 2 integration
    /// point). The epoch must match the current `ExecutionPlan::epoch`.
    pub fn new(current_epoch: PlanEpoch, budget: ArtifactBudget) -> Self {
        Self {
            current_epoch,
            budget,
            accumulator: FrameAccumulator {
                total_bytes: 0,
                total_rows: 0,
            },
            staged: Vec::new(),
            time: 0.0,
            dt: 0.0,
        }
    }

    /// Read the epoch this HostApi is staging for.
    pub fn epoch(&self) -> PlanEpoch {
        self.current_epoch
    }

    /// Set timing for native addons.
    pub fn set_timing(&mut self, time: f32, dt: f32) {
        self.time = time;
        self.dt = dt;
    }

    /// Get current frame time.
    pub fn time(&self) -> f32 {
        self.time
    }

    /// Get delta time.
    pub fn dt(&self) -> f32 {
        self.dt
    }

    /// Publish a `RenderArtifact` for the current frame.
    ///
    /// Validation is **synchronous** (§7 of `plan.md`): the artifact is checked
    /// against its own schema rules and against the per-frame budget before
    /// being staged. On any error the artifact is dropped and `Err` is returned;
    /// the frame continues without interruption (**I017**).
    ///
    /// # Ownership and I012
    /// Takes `artifact` by value. After this call returns `Ok`, the caller
    /// no longer holds the artifact — Rust ownership enforces **I012** at
    /// compile time (cross-frame reuse is impossible without cloning, and
    /// `RenderArtifact` is intentionally `!Clone`).
    ///
    /// # Epoch check
    /// If `artifact_epoch` does not match the `HostApi`'s `current_epoch`, the
    /// artifact is dropped with `PublishError::StaleEpoch`. An `ExecutionUnit`
    /// that lags behind frame pacing will have its output silently discarded
    /// rather than corrupting graph ordering.
    pub fn publish_artifact(
        &mut self,
        instance_id: String,
        artifact: RenderArtifact,
        artifact_epoch: PlanEpoch,
    ) -> Result<(), PublishError> {
        // Epoch guard — cross-epoch artifacts are forbidden (I012).
        if artifact_epoch != self.current_epoch {
            return Err(PublishError::StaleEpoch {
                artifact_epoch,
                current_epoch: self.current_epoch,
            });
        }

        // Schema validation (§7 — synchronous, before reaching broker).
        artifact.validate().map_err(PublishError::Validation)?;

        // Per-artifact budget check (I017).
        self.budget
            .check_artifact(&artifact)
            .map_err(PublishError::BudgetExceeded)?;

        // Frame-total budget check (I017).
        let artifact_bytes = artifact.estimated_bytes();
        let new_total = self.accumulator.total_bytes.saturating_add(artifact_bytes);
        if new_total > self.budget.max_frame_bytes {
            return Err(PublishError::BudgetExceeded(
                ArtifactValidationError::FrameBudgetExceeded {
                    bytes: new_total,
                    limit: self.budget.max_frame_bytes,
                },
            ));
        }

        self.accumulator.total_bytes = new_total;
        
        // Last-write-wins: if this instance_id already published this frame, overwrite it.
        if let Some(existing) = self.staged.iter_mut().find(|s| s.instance_id == instance_id) {
            #[cfg(debug_assertions)]
            eprintln!("[host_api] warning: multiple artifacts published for instance {}", instance_id);
            existing.artifact = artifact;
        } else {
            self.staged.push(StagedArtifact {
                instance_id,
                artifact,
                epoch: self.current_epoch,
            });
        }

        Ok(())
    }

    /// Consume the `HostApi`, returning all successfully staged artifacts.
    ///
    /// Called by `PipelineRuntime` at the end of the prepare phase to hand
    /// the staged artifacts to the `ResourceBroker` (Phase 4). After this call
    /// the frame's publish window is closed.
    pub fn drain_staged(self) -> Vec<StagedArtifact> {
        self.staged
    }

    /// The number of artifacts successfully staged this frame.
    pub fn staged_count(&self) -> usize {
        self.staged.len()
    }

    /// The running byte total of all staged artifacts this frame.
    pub fn staged_bytes(&self) -> usize {
        self.accumulator.total_bytes
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::artifact::{
        ArtifactBudget, InstanceSchema, RenderArtifact, SemanticField, SemanticRow, SemanticRows,
        SemanticValue,
    };

    fn valid_instances_artifact() -> RenderArtifact {
        RenderArtifact::Instances {
            schema: InstanceSchema {
                schema_id: 1,
                fields: vec![SemanticField::Position2],
            },
            rows: SemanticRows {
                schema_id: 1,
                rows: vec![SemanticRow {
                    values: vec![SemanticValue::Vec2([0.0, 0.0])],
                }],
            },
        }
    }

    #[test]
    fn valid_artifact_stages_successfully() {
        let epoch = PlanEpoch(1);
        let mut api = HostApi::new(epoch, ArtifactBudget::default());
        assert!(api.publish_artifact("test1".into(), valid_instances_artifact(), epoch).is_ok());
        assert_eq!(api.staged_count(), 1);
    }

    #[test]
    fn stale_epoch_is_rejected() {
        let current = PlanEpoch(2);
        let stale = PlanEpoch(1);
        let mut api = HostApi::new(current, ArtifactBudget::default());
        let err = api
            .publish_artifact("test2".into(), valid_instances_artifact(), stale)
            .unwrap_err();
        assert!(matches!(
            err,
            PublishError::StaleEpoch {
                artifact_epoch,
                current_epoch
            } if artifact_epoch == stale && current_epoch == current
        ));
    }

    #[test]
    fn malformed_artifact_is_rejected_before_staging() {
        let epoch = PlanEpoch(1);
        let mut api = HostApi::new(epoch, ArtifactBudget::default());
        let bad = RenderArtifact::Instances {
            schema: InstanceSchema {
                schema_id: 0, // violates I011
                fields: vec![],
            },
            rows: SemanticRows {
                schema_id: 0,
                rows: vec![],
            },
        };
        assert!(matches!(
            api.publish_artifact("test3".into(), bad, epoch),
            Err(PublishError::Validation(
                ArtifactValidationError::UnversionedSchema
            ))
        ));
        assert_eq!(api.staged_count(), 0, "bad artifact must not be staged");
    }

    #[test]
    fn frame_budget_exceeded_is_rejected() {
        let epoch = PlanEpoch(1);
        let tiny_budget = ArtifactBudget {
            max_artifact_bytes: 1000,
            max_frame_bytes: 10, // only 10 bytes for the whole frame
            max_frame_rows: 1000,
        };
        let mut api = HostApi::new(epoch, tiny_budget);
        // valid_instances_artifact() has 1 row × Vec2 = 8 bytes.
        assert!(api.publish_artifact("test".into(), valid_instances_artifact(), epoch).is_ok());
        // Second one would push total to 16 > 10.
        assert!(matches!(
            api.publish_artifact("test2".into(), valid_instances_artifact(), epoch),
            Err(PublishError::BudgetExceeded(
                ArtifactValidationError::FrameBudgetExceeded { .. }
            ))
        ));
        assert_eq!(api.staged_count(), 1, "second artifact must not be staged");
    }

    #[test]
    fn drain_staged_returns_all_valid_artifacts() {
        let epoch = PlanEpoch(1);
        let mut api = HostApi::new(epoch, ArtifactBudget::default());
        api.publish_artifact("test3".into(), valid_instances_artifact(), epoch).unwrap();
        api.publish_artifact("test4".into(), RenderArtifact::None, epoch).unwrap();
        let drained = api.drain_staged();
        assert_eq!(drained.len(), 2);
    }
}
