//! [`MockRunnerBackend`] — the in-process proving harness (no subprocess).
//!
//! It materializes an [`ExecutionUnit`] at `load` (per F4) from either a
//! `BehaviorFactory` (→ [`BehaviorExecutionUnit`], driving a real node — the
//! `Factory → ExecutionUnit → HostApi → BehaviorNode.update()` proof) or a
//! prebuilt unit (e.g. a [`MockExecutionUnit`] for wiring/supervisor tests). It
//! is "mock" only in that it runs in-process and is never wired into the live
//! engine — the runner stays dormant.

use crate::addon::schema::ParamMap;
use crate::behavior::BehaviorFactory;

use super::backend::{LoadedRunner, RunnerBackend};
use super::execution::{BehaviorExecutionUnit, ExecutionUnit};
use super::{RunnerError, RunnerKind};

/// What a [`MockRunnerBackend`] will turn into a unit at `load`.
enum Source {
    /// Build a [`BehaviorExecutionUnit`] from a factory (construction at `load`).
    Behavior {
        factory: BehaviorFactory,
        instance_id: String,
        config: ParamMap,
        enabled: bool,
    },
    /// Use a unit prepared by the caller (e.g. a [`MockExecutionUnit`]).
    Prebuilt(Box<dyn ExecutionUnit>),
}

pub struct MockRunnerBackend {
    source: Source,
}

impl MockRunnerBackend {
    /// Drive a real behavior through the seam.
    pub fn from_factory(
        factory: BehaviorFactory,
        instance_id: impl Into<String>,
        config: ParamMap,
        enabled: bool,
    ) -> Self {
        Self {
            source: Source::Behavior {
                factory,
                instance_id: instance_id.into(),
                config,
                enabled,
            },
        }
    }

    /// Drive a caller-supplied unit (wiring/supervisor proofs).
    pub fn with_unit(unit: Box<dyn ExecutionUnit>) -> Self {
        Self {
            source: Source::Prebuilt(unit),
        }
    }
}

impl RunnerBackend for MockRunnerBackend {
    fn kind(&self) -> RunnerKind {
        RunnerKind::InProcessRust // mock runs in-process
    }

    fn load(self: Box<Self>) -> Result<LoadedRunner, RunnerError> {
        let unit: Box<dyn ExecutionUnit> = match self.source {
            Source::Behavior {
                factory,
                instance_id,
                config,
                enabled,
            } => Box::new(BehaviorExecutionUnit::from_init(factory(
                instance_id,
                config,
                enabled,
            ))),
            Source::Prebuilt(unit) => unit,
        };
        Ok(LoadedRunner::new(RunnerKind::InProcessRust, unit))
    }
}

// #[cfg(test)]
// mod tests {
//     ...
// }
