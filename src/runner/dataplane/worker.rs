//! [`WorkerBridge`] — realizes `spawn_worker()`, preserving the frozen Step-2.6
//! rule: a worker is **owned by the runner**; the host only **tracks** it. The
//! bridge **only exposes capability** — it hands out opaque [`WorkerHandle`]s when
//! granted and counts the live ones. **depth = 1**: there is no API here to spawn
//! a runner, so a worker can never become a nested runner.

use crate::runner::host::{CapabilityDenied, WorkerHandle};

pub struct WorkerBridge {
    granted: bool,
    next: u32,
    live: u32,
}

impl WorkerBridge {
    pub fn new(granted: bool) -> Self {
        Self {
            granted,
            next: 1,
            live: 0,
        }
    }

    /// Hand out a worker handle if the capability is granted. The runner owns the
    /// worker; the host only records the handle. Deny-by-default.
    pub fn spawn(&mut self) -> Result<WorkerHandle, CapabilityDenied> {
        if !self.granted {
            return Err(CapabilityDenied {
                capability: "spawn_worker",
            });
        }
        let handle = WorkerHandle(self.next);
        self.next += 1;
        self.live += 1;
        Ok(handle)
    }

    /// How many workers the host is tracking for this runner (diagnostics).
    pub fn live(&self) -> u32 {
        self.live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granted_capability_hands_out_unique_tracked_handles() {
        let mut w = WorkerBridge::new(true);
        let a = w.spawn().unwrap();
        let b = w.spawn().unwrap();
        assert_ne!(a, b, "handles are unique");
        assert_eq!(w.live(), 2, "host tracks the live count");
    }

    #[test]
    fn denied_by_default() {
        let mut w = WorkerBridge::new(false);
        assert_eq!(w.spawn().unwrap_err().capability, "spawn_worker");
        assert_eq!(w.live(), 0);
    }

    // depth = 1 is structural: `WorkerBridge` exposes only `spawn() -> WorkerHandle`.
    // There is deliberately no method that yields a runner, so a worker cannot
    // become a nested runner. (Enforced by absence, not a runtime check.)
}
