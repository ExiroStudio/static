//! [`Supervisor`] — the policy seam: watchdog + fault/restart/breaker state.
//!
//! Owns *when* to restart or disable a runner; owns **no** transport, no runner
//! internals, no execution. Pure, deterministic logic (time is passed in, never
//! read — the codebase forbids ambient clocks on these paths), so it is fully
//! unit-testable without spawning anything.
//!
//! Crash machine (RFC §Q5):
//! ```text
//!   Loading → Ready → Running ⇄ Stopped → Unloaded
//!      └────────── Faulted ──(backoff)──▶ Restarting ─▶ Loading
//!                    └──(breaker: N faults / window T)──▶ Disabled
//! ```
//! The breaker decision is intended to **persist across a pipeline reload** (so a
//! crash-looping addon cannot re-arm on every config edit); wiring that
//! persistence is a later step — Step 1 implements the machine itself.

use super::clock::Clock;
use super::RunnerState;

/// Restart/breaker tuning. Defaults are placeholders (RFC marks exact values as
/// implementation-tier constants).
#[derive(Debug, Clone, Copy)]
pub struct RestartPolicy {
    /// Faults within `window_ms` that trip the breaker into `Disabled`.
    pub max_faults: u32,
    /// Sliding window for counting faults.
    pub window_ms: u64,
    /// First backoff; doubles each consecutive fault.
    pub backoff_base_ms: u64,
    /// Backoff ceiling.
    pub backoff_cap_ms: u64,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_faults: 3,
            window_ms: 10_000,
            backoff_base_ms: 200,
            backoff_cap_ms: 5_000,
        }
    }
}

/// What the supervisor decides after a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultDecision {
    /// Restart after waiting `backoff_ms`.
    Restart { backoff_ms: u64 },
    /// Breaker tripped — stay down until a manual [`reset`](Supervisor::reset).
    Disable,
}

/// Tracks one runner's lifecycle + fault history and decides restart vs. disable.
///
/// Time comes from an injected [`Clock`] (F3) — never an ambient `Instant::now()`
/// inside this policy — so tests are deterministic.
pub struct Supervisor {
    policy: RestartPolicy,
    clock: Box<dyn Clock>,
    state: RunnerState,
    /// Faults counted in the current window.
    fault_count: u32,
    /// Start of the current fault window (from the injected clock).
    window_start_ms: u64,
}

impl Supervisor {
    pub fn new(policy: RestartPolicy, clock: Box<dyn Clock>) -> Self {
        Self {
            policy,
            clock,
            state: RunnerState::Loading,
            fault_count: 0,
            window_start_ms: 0,
        }
    }

    pub fn state(&self) -> RunnerState {
        self.state
    }

    pub fn is_disabled(&self) -> bool {
        self.state == RunnerState::Disabled
    }

    // ---- nominal lifecycle transitions ----

    pub fn on_loaded(&mut self) {
        if self.state != RunnerState::Disabled {
            self.state = RunnerState::Ready;
        }
    }

    pub fn on_started(&mut self) {
        if matches!(self.state, RunnerState::Ready | RunnerState::Stopped) {
            self.state = RunnerState::Running;
        }
    }

    pub fn on_stopped(&mut self) {
        if self.state == RunnerState::Running {
            self.state = RunnerState::Stopped;
        }
    }

    pub fn on_unloaded(&mut self) {
        if self.state != RunnerState::Disabled {
            self.state = RunnerState::Unloaded;
        }
    }

    // ---- fault handling ----

    /// Record a fault (time read from the injected [`Clock`]). Resets the window
    /// if it elapsed, then either schedules a backed-off restart or trips the
    /// breaker.
    pub fn on_fault(&mut self) -> FaultDecision {
        let now_ms = self.clock.now_ms();
        // Slide the window.
        if self.fault_count == 0 || now_ms.saturating_sub(self.window_start_ms) > self.policy.window_ms {
            self.window_start_ms = now_ms;
            self.fault_count = 0;
        }
        self.fault_count += 1;
        self.state = RunnerState::Faulted;

        if self.fault_count >= self.policy.max_faults {
            self.state = RunnerState::Disabled;
            FaultDecision::Disable
        } else {
            // Exponential backoff, capped.
            let shift = self.fault_count.saturating_sub(1).min(31);
            let backoff_ms = self
                .policy
                .backoff_base_ms
                .saturating_mul(1u64 << shift)
                .min(self.policy.backoff_cap_ms);
            self.state = RunnerState::Restarting;
            FaultDecision::Restart { backoff_ms }
        }
    }

    /// Begin a restart (after the backoff elapsed): back to `Loading`.
    pub fn on_restart_begin(&mut self) {
        if self.state == RunnerState::Restarting {
            self.state = RunnerState::Loading;
        }
    }

    /// Manual re-enable from `Disabled` (clears the breaker). This is the only
    /// way out of `Disabled`.
    pub fn reset(&mut self) {
        self.state = RunnerState::Loading;
        self.fault_count = 0;
        self.window_start_ms = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::clock::ManualClock;

    fn with_clock(policy: RestartPolicy) -> (Supervisor, ManualClock) {
        let clock = ManualClock::new(0);
        let s = Supervisor::new(policy, Box::new(clock.clone()));
        (s, clock)
    }

    #[test]
    fn nominal_lifecycle_transitions() {
        let (mut s, _clock) = with_clock(RestartPolicy::default());
        assert_eq!(s.state(), RunnerState::Loading);
        s.on_loaded();
        assert_eq!(s.state(), RunnerState::Ready);
        s.on_started();
        assert_eq!(s.state(), RunnerState::Running);
        s.on_stopped();
        assert_eq!(s.state(), RunnerState::Stopped);
        s.on_started(); // re-start from Stopped
        assert_eq!(s.state(), RunnerState::Running);
        s.on_unloaded();
        assert_eq!(s.state(), RunnerState::Unloaded);
    }

    #[test]
    fn faults_back_off_then_trip_the_breaker() {
        let (mut s, clock) = with_clock(RestartPolicy {
            max_faults: 3,
            window_ms: 10_000,
            backoff_base_ms: 200,
            backoff_cap_ms: 5_000,
        });

        // Fault 1 → restart, 200ms.
        clock.set(0);
        assert_eq!(s.on_fault(), FaultDecision::Restart { backoff_ms: 200 });
        assert_eq!(s.state(), RunnerState::Restarting);
        // Fault 2 (same window) → restart, 400ms.
        clock.set(100);
        assert_eq!(s.on_fault(), FaultDecision::Restart { backoff_ms: 400 });
        // Fault 3 (same window) → breaker trips.
        clock.set(200);
        assert_eq!(s.on_fault(), FaultDecision::Disable);
        assert!(s.is_disabled());
    }

    #[test]
    fn window_reset_avoids_tripping_on_spread_out_faults() {
        let (mut s, clock) = with_clock(RestartPolicy::default());
        clock.set(0);
        assert!(matches!(s.on_fault(), FaultDecision::Restart { .. }));
        // Far outside the 10s window → counter resets, still a restart.
        clock.set(20_000);
        assert!(matches!(s.on_fault(), FaultDecision::Restart { .. }));
        clock.set(20_100);
        assert!(matches!(s.on_fault(), FaultDecision::Restart { .. }));
        assert!(!s.is_disabled(), "spread-out faults must not trip the breaker");
    }

    #[test]
    fn backoff_is_capped() {
        let (mut s, clock) = with_clock(RestartPolicy {
            max_faults: 100,
            window_ms: u64::MAX,
            backoff_base_ms: 1_000,
            backoff_cap_ms: 4_000,
        });
        let mut last = 0;
        for i in 0..10 {
            clock.set(i);
            if let FaultDecision::Restart { backoff_ms } = s.on_fault() {
                last = backoff_ms;
            }
        }
        assert_eq!(last, 4_000, "backoff must saturate at the cap");
    }

    #[test]
    fn disabled_only_clears_on_manual_reset() {
        let (mut s, _clock) = with_clock(RestartPolicy {
            max_faults: 1,
            ..RestartPolicy::default()
        });
        assert_eq!(s.on_fault(), FaultDecision::Disable);
        assert!(s.is_disabled());
        // Nominal callbacks do NOT revive a disabled runner.
        s.on_loaded();
        s.on_started();
        assert!(s.is_disabled());
        // Only an explicit reset does.
        s.reset();
        assert_eq!(s.state(), RunnerState::Loading);
    }
}
