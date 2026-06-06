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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::builtins::time;
    use crate::behavior::node::Timing;
    use crate::runner::execution::MockExecutionUnit;
    use crate::runner::host::{NullHost, RecordingHost};
    use crate::runner::TickOutcome;
    use crate::signal::{SignalKind, SignalSchema, SignalValue};
    use std::sync::Arc;

    /// The headline proof: BehaviorFactory → ExecutionUnit → MockRunner → HostApi
    /// → BehaviorNode.update(), end to end, with the node's output observed on the
    /// host. Uses the `time` behavior (publishes signal.time = sin(elapsed)).
    #[test]
    fn factory_to_runner_to_host_drives_real_behavior_node() {
        // Host resolves "signal.time" → slot 0.
        let schema = Arc::new(SignalSchema::from_pairs(&[("signal.time", SignalKind::F32)]));
        let mut host = RecordingHost::with_schema(schema)
            .with_timing(Timing {
                dt: 0.0,
                elapsed: std::f32::consts::FRAC_PI_2, // sin = 1.0
            });

        let backend: Box<dyn RunnerBackend> =
            Box::new(MockRunnerBackend::from_factory(time::init_with, "beh-time", ParamMap::new(), true));

        // Typestate flow — illegal orderings would not compile.
        let loaded = backend.load().expect("load");
        let bound = loaded.bind(&mut host);
        assert_eq!(bound.handshake().publish.len(), 1, "tiny handshake carries the publish id");
        let mut running = bound.start(&mut host);

        let outcome = running.tick(&mut host, host_timing());
        assert_eq!(outcome, TickOutcome::Ok);

        // The real node published sin(π/2) = 1.0 → mirrored onto the host.
        let (slot, value) = *host.published.last().expect("node published through the host");
        assert_eq!(slot, 0);
        match value {
            SignalValue::F32(x) => assert!((x - 1.0).abs() < 1e-5, "got {x}"),
            other => panic!("expected F32, got {other:?}"),
        }

        let _stopped = running.stop(&mut host);
    }

    fn host_timing() -> Timing {
        Timing {
            dt: 0.033,
            elapsed: std::f32::consts::FRAC_PI_2,
        }
    }

    /// Wiring proof with a controllable unit: lifecycle is observed and the host
    /// surface is consumed (heartbeat/request_frame), no real behavior involved.
    #[test]
    fn mock_unit_exercises_lifecycle_and_host_surface() {
        let unit = MockExecutionUnit::new(vec!["x".into()]);
        let backend: Box<dyn RunnerBackend> = Box::new(MockRunnerBackend::with_unit(Box::new(unit)));

        let mut host = RecordingHost::new(); // no schema → subscribe returns None
        let loaded = backend.load().unwrap();
        let mut running = loaded.bind(&mut host).start(&mut host);

        assert_eq!(running.tick(&mut host, host_timing()), TickOutcome::Ok);
        assert_eq!(running.tick(&mut host, host_timing()), TickOutcome::Ok);
        let _stopped = running.stop(&mut host);

        assert_eq!(host.heartbeats, 2, "unit consumed host.heartbeat each run");
        assert_eq!(host.frame_requests, 2, "unit consumed host.request_frame each run");
        assert!(host.published.is_empty(), "no schema → subscribe None → no publish");
    }

    /// NullHost still satisfies the surface end to end (deny-by-default).
    #[test]
    fn null_host_still_drives_the_flow() {
        let unit = MockExecutionUnit::new(vec!["x".into()]);
        let backend: Box<dyn RunnerBackend> = Box::new(MockRunnerBackend::with_unit(Box::new(unit)));
        let mut host = NullHost;
        let mut running = backend.load().unwrap().bind(&mut host).start(&mut host);
        assert_eq!(running.tick(&mut host, host_timing()), TickOutcome::Ok);
        let _ = running.stop(&mut host);
    }

    /// A faulting unit surfaces `Faulted` for the supervisor to act on.
    #[test]
    fn faulting_unit_reports_faulted_outcome() {
        let unit = MockExecutionUnit::new(vec![]).faulting_after(1);
        let backend: Box<dyn RunnerBackend> = Box::new(MockRunnerBackend::with_unit(Box::new(unit)));
        let mut host = NullHost;
        let mut running = backend.load().unwrap().bind(&mut host).start(&mut host);
        assert_eq!(running.tick(&mut host, host_timing()), TickOutcome::Ok); // run 1
        assert!(
            matches!(running.tick(&mut host, host_timing()), TickOutcome::Faulted(_)),
            "run 2 faults"
        );
    }
}
