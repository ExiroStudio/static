//! [`RunnerBackend`] + the **typestate** lifecycle (Step-1 feedback F4).
//!
//! Step 1 had one struct with `&mut self` lifecycle methods and a runtime state
//! enum; illegal orderings were runtime errors. Step 2 makes them *unrepresentable*:
//!
//! ```text
//!   Box<dyn RunnerBackend>.load() ─▶ LoadedRunner
//!                          .bind(host) ─▶ BoundRunner   (Handshake now available)
//!                          .start(host) ─▶ RunningRunner
//!                          .tick(host, timing)* ─▶ TickOutcome
//!                          .stop(host) ─▶ StoppedRunner ─▶ start | unload
//! ```
//!
//! You cannot `tick` what isn't `Running`, nor `start` before `bind` — those are
//! type errors. Each state owns a `Box<dyn ExecutionUnit>` (F1): no `BehaviorNode`
//! lives in any runner type.

use crate::behavior::node::Timing;

use super::execution::ExecutionUnit;
use super::host::HostApi;
use super::{Capabilities, Handshake, RunnerError, RunnerKind, TickOutcome, ABI_VERSION};

/// Produces a [`LoadedRunner`]. The *only* trait method that needs dynamic
/// dispatch — everything after `load` is a concrete typestate value.
pub trait RunnerBackend {
    fn kind(&self) -> RunnerKind;
    /// Construct the execution unit and enter the `Loaded` state. No execution.
    fn load(self: Box<Self>) -> Result<LoadedRunner, RunnerError>;
}

/// Loaded: the unit exists, nothing has run, no host is bound yet.
pub struct LoadedRunner {
    kind: RunnerKind,
    caps: Capabilities,
    unit: Box<dyn ExecutionUnit>,
}

impl LoadedRunner {
    pub fn new(kind: RunnerKind, unit: Box<dyn ExecutionUnit>) -> Self {
        let caps = unit.caps();
        Self { kind, caps, unit }
    }

    pub fn kind(&self) -> RunnerKind {
        self.kind
    }

    /// Published signal names the unit declares (internal; not the ABI handshake).
    pub fn declares_publish(&self) -> &[String] {
        self.unit.publishes()
    }

    /// Bind the host surface, resolving publish/consume names → ids and building
    /// the tiny [`Handshake`]. Consumes `self` → `BoundRunner` (can't be re-bound).
    pub fn bind(mut self, host: &mut dyn HostApi) -> BoundRunner {
        let publish = self.unit.publishes().iter().filter_map(|n| host.subscribe(n)).collect();
        let consume = self.unit.consumes().iter().filter_map(|n| host.subscribe(n)).collect();
        self.unit.bind(host);
        let handshake = Handshake {
            version: ABI_VERSION,
            caps: self.caps,
            publish,
            consume,
        };
        BoundRunner {
            kind: self.kind,
            unit: self.unit,
            handshake,
        }
    }

    /// Discard without ever binding/running.
    pub fn unload(self) {}
}

/// Bound: a host is attached and the [`Handshake`] is fixed; not yet running.
pub struct BoundRunner {
    kind: RunnerKind,
    unit: Box<dyn ExecutionUnit>,
    handshake: Handshake,
}

impl BoundRunner {
    pub fn kind(&self) -> RunnerKind {
        self.kind
    }
    pub fn handshake(&self) -> &Handshake {
        &self.handshake
    }
    /// Begin running.
    pub fn start(mut self, host: &mut dyn HostApi) -> RunningRunner {
        self.unit.start(host);
        RunningRunner {
            kind: self.kind,
            unit: self.unit,
            handshake: self.handshake,
        }
    }
    pub fn unload(self) {}
}

/// Running: the only state that can `tick`.
pub struct RunningRunner {
    kind: RunnerKind,
    unit: Box<dyn ExecutionUnit>,
    handshake: Handshake,
}

impl RunningRunner {
    pub fn kind(&self) -> RunnerKind {
        self.kind
    }
    pub fn handshake(&self) -> &Handshake {
        &self.handshake
    }
    /// One step of work.
    pub fn tick(&mut self, host: &mut dyn HostApi, timing: Timing) -> TickOutcome {
        self.unit.run(host, timing)
    }
    /// Pause; keep the unit for a fast re-`start`.
    pub fn stop(mut self, host: &mut dyn HostApi) -> StoppedRunner {
        self.unit.stop(host);
        StoppedRunner {
            kind: self.kind,
            unit: self.unit,
            handshake: self.handshake,
        }
    }
}

/// Stopped: paused but loaded. Can restart or unload — but not tick (type error).
pub struct StoppedRunner {
    kind: RunnerKind,
    unit: Box<dyn ExecutionUnit>,
    handshake: Handshake,
}

impl StoppedRunner {
    pub fn kind(&self) -> RunnerKind {
        self.kind
    }
    pub fn handshake(&self) -> &Handshake {
        &self.handshake
    }
    pub fn start(mut self, host: &mut dyn HostApi) -> RunningRunner {
        self.unit.start(host);
        RunningRunner {
            kind: self.kind,
            unit: self.unit,
            handshake: self.handshake,
        }
    }
    pub fn unload(self) {}
}
