//! Runner — execution seams (Phase 3b, Steps 1–2).
//!
//! Introduces the addon execution abstractions with **no runtime behavior
//! change**. Everything here is a *seam*: defined, unit-tested, and **dormant** —
//! not wired into the live behavior path. The engine still creates behaviors
//! through [`BehaviorHost::create_inits`](crate::behavior::BehaviorHost) and
//! drives them on the unchanged [`BehaviorScheduler`](crate::behavior).
//!
//! ```text
//!   Engine ─ Scheduler ─ BehaviorNode                  (unchanged, live)
//!
//!   RunnerBackend.load() ─▶ LoadedRunner ─bind─▶ BoundRunner ─start─▶ RunningRunner
//!        (mechanism)                                   │ owns Box<dyn ExecutionUnit>
//!   Supervisor (policy: watchdog + fault/restart/breaker, injected Clock)
//!   HostApi (capability surface) · Sandbox (trait + placeholders)
//! ```
//!
//! **Step 2** added the mock execution flow: a typestate lifecycle (illegal
//! transitions are type errors), an [`ExecutionUnit`] abstraction so no runner
//! holds a `BehaviorNode`, a tiny ids-only [`Handshake`], an injected [`Clock`],
//! and [`MockRunnerBackend`]/[`RecordingHost`] proving
//! `BehaviorFactory → ExecutionUnit → HostApi → BehaviorNode.update()` — all
//! in-process, test-only, no subprocess/protocol/shmem/sandbox execution.

// Step 1–2 seams: defined + unit-tested, wired onto the live path in Step 3+.
// Until then the types are unreferenced by the live binary and the public
// re-exports have no in-crate consumer — both are intentional, not stale.
#![allow(dead_code)]
#![allow(unused_imports)]

mod backend;
mod clock;
mod dataplane;
mod execution;
mod host;
mod inproc;
mod mock;
mod native;
mod sandbox;
mod supervisor;

pub use backend::{BoundRunner, LoadedRunner, RunnerBackend, RunningRunner, StoppedRunner};
pub use clock::{Clock, ManualClock, SystemClock};
pub use dataplane::{
    BridgedHost, FrameBridge, MetricsBridge, ResourceBridge, SignalBridge, WorkerBridge,
};
pub use execution::{BehaviorExecutionUnit, ExecutionUnit, MockExecutionUnit};
pub use host::{
    CapabilityDenied, FrameRef, FrameTier, HostApi, LogLevel, Metrics, NullHost, RecordingHost,
    ResourceHandle, WorkerHandle,
};
pub use inproc::InProcessRustRunner;
pub use mock::MockRunnerBackend;
pub use native::{
    ControlRequest, ControlResponse, ControlTransport, HostBridge, LoopbackTransport,
    NativeExecutionUnit, NativeProcess, NativeRunnerBackend, ProcessSupervisor, ProcessTransport,
    SupervisionReport, TransportError, TransportSpec,
};
pub use sandbox::{LinuxSandbox, MacSandbox, Sandbox, SandboxError, SandboxSpec, WindowsSandbox};
pub use supervisor::{FaultDecision, RestartPolicy, Supervisor};

use crate::signal::SignalId;

/// The boundary ABI version negotiated at the handshake. Append-only; bumped only
/// on a breaking wire change. (No protocol exists yet — this is the version the
/// in-process handshake reports.)
pub const ABI_VERSION: u16 = 1;

/// Which execution backend a runner is. Only the in-process path exists; the
/// reserved variants keep the enum forward-stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    /// First-party Rust, executed in the engine process.
    InProcessRust,
    /// Out-of-process subprocess (Step 3). Diagnostics only — no behavior
    /// branches on this.
    Native,
    // Wasm,    // later: in-VM sandbox
}

/// Lifecycle state owned by the [`Supervisor`] FSM. (The *runner's* own lifecycle
/// is the typestate `LoadedRunner`/`BoundRunner`/`RunningRunner`/`StoppedRunner`,
/// not an enum — F4.) These are the states the policy reasons about, including the
/// fault subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerState {
    Loading,
    Ready,
    Running,
    Stopped,
    Unloaded,
    Faulted,
    Restarting,
    Disabled,
}

/// Capabilities an addon *requests* (handshake field). A declaration only — never
/// enforced here. `gpu_compute` is the addon's own compute context (never the
/// engine's GPU), per the RFC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub network: bool,
    pub filesystem: bool,
    pub camera: bool,
    pub frame_fullres: bool,
    pub gpu_compute: bool,
    pub spawn_worker: bool,
}

/// The tiny, ids-only handshake that crosses the boundary (F2): version, requested
/// capabilities, and the resolved publish/consume **slot ids** — nothing else. No
/// schema, no resource metadata, no signal payload, no frame metadata.
#[derive(Debug, Clone, Default)]
pub struct Handshake {
    pub version: u16,
    pub caps: Capabilities,
    pub publish: Vec<SignalId>,
    pub consume: Vec<SignalId>,
}

/// The result of one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickOutcome {
    /// The runner produced its output for this tick.
    Ok,
    /// Loaded but not executing this tick (used by a dormant/idle runner).
    Idle,
    /// The runner failed; the [`Supervisor`] decides restart vs. disable.
    Faulted(String),
}

/// Errors from constructing a runner. (Lifecycle ordering errors are impossible by
/// construction — they are type errors in the typestate, not runtime variants.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    /// The backend could not build its execution unit.
    Load(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_is_tiny_and_ids_only() {
        let h = Handshake::default();
        assert_eq!(h.version, 0);
        assert!(h.publish.is_empty() && h.consume.is_empty());
        assert_eq!(h.caps, Capabilities::default());
    }

    #[test]
    fn capabilities_default_to_all_denied() {
        let c = Capabilities::default();
        assert!(!c.network && !c.filesystem && !c.camera);
        assert!(!c.frame_fullres && !c.gpu_compute && !c.spawn_worker);
    }
}
