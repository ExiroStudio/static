//! [`ProcessSupervisor`] — restart policy for a native runner.
//!
//! It owns the **frozen** [`Supervisor`] (FSM + injected `Clock`) and a respawn
//! loop. A crash (`TickOutcome::Faulted` / transport `Closed`) is fed to
//! `Supervisor::on_fault`; the resulting [`FaultDecision`] either rebuilds+restarts
//! the runner or trips the breaker to `Disabled`. It changes neither the
//! `Supervisor` nor the typestate — it *drives* them.

use crate::behavior::node::Timing;
use crate::runner::backend::RunnerBackend;
use crate::runner::host::HostApi;
use crate::runner::{FaultDecision, RestartPolicy, RunnerState, Supervisor, TickOutcome};

use crate::runner::clock::Clock;

/// Outcome of a supervised run (for tests/diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisionReport {
    pub attempts: u32,
    pub restarts: u32,
    pub disabled: bool,
    pub last_state: RunnerState,
}

pub struct ProcessSupervisor {
    supervisor: Supervisor,
}

impl ProcessSupervisor {
    pub fn new(policy: RestartPolicy, clock: Box<dyn Clock>) -> Self {
        Self {
            supervisor: Supervisor::new(policy, clock),
        }
    }

    pub fn state(&self) -> RunnerState {
        self.supervisor.state()
    }

    /// Build a runner via `make`, drive it through `load→bind→start→tick…`, and
    /// apply restart policy on any fault. Loops until a clean run completes, the
    /// breaker disables it, or `max_attempts` is reached. No real sleeping — the
    /// injected clock makes backoff deterministic.
    pub fn supervise(
        &mut self,
        make: &mut dyn FnMut() -> Box<dyn RunnerBackend>,
        host: &mut dyn HostApi,
        ticks_per_attempt: u32,
        max_attempts: u32,
    ) -> SupervisionReport {
        let mut report = SupervisionReport {
            attempts: 0,
            restarts: 0,
            disabled: false,
            last_state: self.supervisor.state(),
        };

        while report.attempts < max_attempts {
            report.attempts += 1;

            // load → bind → start (typestate; a load error is itself a fault).
            let loaded = match make().load() {
                Ok(l) => l,
                Err(_) => {
                    if self.on_fault(&mut report) {
                        break;
                    }
                    continue;
                }
            };
            self.supervisor.on_loaded();
            let mut running = loaded.bind(host).start(host);
            self.supervisor.on_started();

            // Tick until a fault or the attempt's tick budget is spent.
            let mut faulted = false;
            for _ in 0..ticks_per_attempt {
                if matches!(running.tick(host, zero_timing()), TickOutcome::Faulted(_)) {
                    faulted = true;
                    break;
                }
            }

            if faulted {
                if self.on_fault(&mut report) {
                    break; // disabled
                }
                // else: Restart → loop and rebuild
            } else {
                let _stopped = running.stop(host); // clean run → graceful stop
                self.supervisor.on_stopped();
                break;
            }
        }

        report.last_state = self.supervisor.state();
        report
    }

    /// Apply one fault to the frozen `Supervisor`. Returns `true` if the breaker
    /// tripped (caller should stop), `false` if a restart was scheduled.
    fn on_fault(&mut self, report: &mut SupervisionReport) -> bool {
        match self.supervisor.on_fault() {
            FaultDecision::Restart { .. } => {
                report.restarts += 1;
                // A real impl would wait `backoff_ms`; the injected clock keeps
                // tests deterministic, so no sleep here.
                self.supervisor.on_restart_begin();
                false
            }
            FaultDecision::Disable => {
                report.disabled = true;
                true
            }
        }
    }
}

fn zero_timing() -> Timing {
    Timing {
        dt: 0.0,
        elapsed: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::clock::ManualClock;
    use crate::runner::host::NullHost;
    use crate::runner::native::NativeRunnerBackend;

    fn policy(max_faults: u32) -> RestartPolicy {
        RestartPolicy {
            max_faults,
            window_ms: 10_000,
            backoff_base_ms: 1,
            backoff_cap_ms: 1,
        }
    }

    /// Crash → Fault → Supervisor → restart, until the breaker disables it.
    /// With the clock held fixed, all faults fall in one window: max_faults=3 ⇒
    /// 2 restarts then Disable.
    #[test]
    fn crashing_runner_restarts_then_trips_breaker() {
        let clock = ManualClock::new(0);
        let mut ps = ProcessSupervisor::new(policy(3), Box::new(clock));
        let mut host = NullHost;
        let mut make = || Box::new(NativeRunnerBackend::loopback_dying(&["x"], 0)) as Box<dyn RunnerBackend>;

        let report = ps.supervise(&mut make, &mut host, 4, 10);
        assert_eq!(report.restarts, 2, "fault1→restart, fault2→restart, fault3→disable");
        assert!(report.disabled);
        assert_eq!(report.last_state, RunnerState::Disabled);
    }

    /// A healthy runner completes its tick budget with no restart, no disable.
    #[test]
    fn healthy_runner_runs_clean() {
        let clock = ManualClock::new(0);
        let mut ps = ProcessSupervisor::new(policy(3), Box::new(clock));
        let mut host = NullHost;
        let mut make = || Box::new(NativeRunnerBackend::loopback(&["x"])) as Box<dyn RunnerBackend>;

        let report = ps.supervise(&mut make, &mut host, 5, 10);
        assert_eq!(report.attempts, 1);
        assert_eq!(report.restarts, 0);
        assert!(!report.disabled);
    }
}
