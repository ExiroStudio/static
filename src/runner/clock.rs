//! [`Clock`] — injectable time source (Step-1 feedback F3).
//!
//! The [`Supervisor`](super::Supervisor) policy must be deterministic and must
//! never read an ambient clock. It depends on this trait instead; tests drive a
//! [`ManualClock`], production wires a [`SystemClock`]. No `Instant::now()` ever
//! appears *inside* policy.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// A monotonic millisecond time source. The only time dependency policy has.
pub trait Clock {
    fn now_ms(&self) -> u64;
}

/// Deterministic, test-driven clock. Cheap to clone (shares the counter), so a
/// test keeps one handle to `advance`/`set` while another is held by the
/// supervisor.
#[derive(Clone, Default)]
pub struct ManualClock {
    ms: Arc<AtomicU64>,
}

impl ManualClock {
    pub fn new(ms: u64) -> Self {
        Self {
            ms: Arc::new(AtomicU64::new(ms)),
        }
    }
    pub fn set(&self, ms: u64) {
        self.ms.store(ms, Ordering::Relaxed);
    }
    pub fn advance(&self, delta_ms: u64) {
        self.ms.fetch_add(delta_ms, Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.ms.load(Ordering::Relaxed)
    }
}

/// Real monotonic clock. `Instant::now()` lives here — in the injected clock,
/// **not** in policy — so the F3 rule ("no clock inside the supervisor") holds.
pub struct SystemClock {
    base: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.base.elapsed().as_millis() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_is_deterministic_and_shareable() {
        let c = ManualClock::new(100);
        let shared = c.clone();
        assert_eq!(shared.now_ms(), 100);
        c.advance(50);
        assert_eq!(shared.now_ms(), 150, "clones share the same counter");
        c.set(0);
        assert_eq!(shared.now_ms(), 0);
    }

    #[test]
    fn system_clock_is_monotonic_nondecreasing() {
        let c = SystemClock::new();
        let a = c.now_ms();
        let b = c.now_ms();
        assert!(b >= a);
    }
}
