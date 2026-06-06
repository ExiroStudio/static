//! [`ControlTransport`] — the request→response channel to a runner child.
//!
//! Strictly request→response (Step 2.5): one command in, one response out. No
//! stream, no event bus. Heartbeat is implicit (a successful response); Fault is
//! a [`TransportError`] (closed/timeout/protocol). The [`LoopbackTransport`] is a
//! deterministic in-process child that fully implements the protocol + legality,
//! so lifecycle/fault logic is provable without an OS process.

use super::{ControlRequest, ControlResponse, TransportError};
use crate::runner::TickOutcome;

/// A control channel to exactly one runner child. Implementations may be
/// in-process (loopback) or out-of-process (a real subprocess) — the caller
/// cannot tell.
pub trait ControlTransport {
    /// Send one control command and get its response (or a transport error → Fault).
    fn request(&mut self, req: ControlRequest) -> Result<ControlResponse, TransportError>;
    /// Cheap liveness check (used by the supervisor's watchdog).
    fn is_alive(&mut self) -> bool;
}

/// The child's protocol state, used to enforce the Step-2.5 legality matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildState {
    NotLoaded,
    Loaded,
    Bound,
    Started,
    Stopped,
    Unloaded,
}

/// A deterministic, in-process "child" implementing the control protocol. Used to
/// prove lifecycle, legality, heartbeat (implicit), and fault propagation without
/// spawning a process. Configurable to fail at start or die after N ticks.
pub struct LoopbackTransport {
    state: ChildState,
    alive: bool,
    ticks: u32,
    die_after: Option<u32>,
    fail_start: bool,
}

impl LoopbackTransport {
    pub fn new() -> Self {
        Self {
            state: ChildState::NotLoaded,
            alive: true,
            ticks: 0,
            die_after: None,
            fail_start: false,
        }
    }
    /// Child dies (channel closes) once `ticks` exceeds `n`.
    pub fn dying_after(mut self, n: u32) -> Self {
        self.die_after = Some(n);
        self
    }
    /// Child fails to start (Start → Closed) — start-failure semantics.
    pub fn failing_start(mut self) -> Self {
        self.fail_start = true;
        self
    }
}

impl Default for LoopbackTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlTransport for LoopbackTransport {
    fn request(&mut self, req: ControlRequest) -> Result<ControlResponse, TransportError> {
        if !self.alive {
            return Err(TransportError::Closed);
        }
        use ChildState::*;
        use ControlRequest as Q;
        match (&req, self.state) {
            (Q::Load, NotLoaded) => {
                self.state = Loaded;
                Ok(ControlResponse::Loaded)
            }
            (Q::Bind, Loaded) => {
                self.state = Bound;
                Ok(ControlResponse::Bound)
            }
            (Q::Start, Bound) | (Q::Start, Stopped) => {
                if self.fail_start {
                    self.alive = false; // start failure surfaces as a closed channel
                    return Err(TransportError::Closed);
                }
                self.state = Started;
                Ok(ControlResponse::Started)
            }
            (Q::Tick { .. }, Started) => {
                self.ticks += 1;
                if matches!(self.die_after, Some(n) if self.ticks > n) {
                    self.alive = false;
                    return Err(TransportError::Closed); // crash mid-run
                }
                Ok(ControlResponse::Ticked(TickOutcome::Ok))
            }
            (Q::Stop, Started) => {
                self.state = Stopped;
                Ok(ControlResponse::Stopped)
            }
            (Q::Unload, _) => {
                self.state = Unloaded;
                Ok(ControlResponse::Unloaded)
            }
            // Everything else is illegal for the current state (legality matrix).
            _ => Err(TransportError::Protocol("illegal control message for state")),
        }
    }

    fn is_alive(&mut self) -> bool {
        self.alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_follows_the_lifecycle() {
        let mut t = LoopbackTransport::new();
        assert_eq!(t.request(ControlRequest::Load).unwrap(), ControlResponse::Loaded);
        assert_eq!(t.request(ControlRequest::Bind).unwrap(), ControlResponse::Bound);
        assert_eq!(t.request(ControlRequest::Start).unwrap(), ControlResponse::Started);
        assert_eq!(
            t.request(ControlRequest::tick(crate::behavior::node::Timing { dt: 0.0, elapsed: 0.0 })).unwrap(),
            ControlResponse::Ticked(TickOutcome::Ok)
        );
        assert_eq!(t.request(ControlRequest::Stop).unwrap(), ControlResponse::Stopped);
        assert!(t.is_alive());
    }

    #[test]
    fn loopback_enforces_legality() {
        let mut t = LoopbackTransport::new();
        // Tick before Start is illegal.
        assert_eq!(
            t.request(ControlRequest::tick(crate::behavior::node::Timing { dt: 0.0, elapsed: 0.0 })),
            Err(TransportError::Protocol("illegal control message for state"))
        );
    }

    #[test]
    fn loopback_can_crash_mid_run() {
        let mut t = LoopbackTransport::new().dying_after(0);
        t.request(ControlRequest::Load).unwrap();
        t.request(ControlRequest::Bind).unwrap();
        t.request(ControlRequest::Start).unwrap();
        let tick = ControlRequest::tick(crate::behavior::node::Timing { dt: 0.0, elapsed: 0.0 });
        assert_eq!(t.request(tick), Err(TransportError::Closed));
        assert!(!t.is_alive(), "crashed child is no longer alive");
    }

    #[test]
    fn loopback_can_fail_start() {
        let mut t = LoopbackTransport::new().failing_start();
        t.request(ControlRequest::Load).unwrap();
        t.request(ControlRequest::Bind).unwrap();
        assert_eq!(t.request(ControlRequest::Start), Err(TransportError::Closed));
    }
}
