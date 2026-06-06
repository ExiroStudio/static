//! [`NativeRunnerBackend`] + [`NativeExecutionUnit`] — the subprocess realization
//! behind the frozen `RunnerBackend`/`ExecutionUnit` seam.
//!
//! Lifecycle mapping (frozen rules):
//! * `load` — metadata only (no spawn, no model, no GPU, no frame).
//! * `bind` — host binding only (forwards `subscribe → host` via [`HostBridge`]).
//! * `start` — heavy init: spawn the child + control handshake (Load/Bind/Start).
//! * `tick` — one budgeted control round-trip → `TickOutcome` (never blocks the host).
//! * `stop` — graceful Stop/Unload, then terminate + reap.
//!
//! The runner owns **process lifecycle only**. It never owns signals/frames/
//! resources/metrics; it forwards. No local cache, no shadow state.

use crate::behavior::node::Timing;
use crate::runner::backend::{LoadedRunner, RunnerBackend};
use crate::runner::execution::ExecutionUnit;
use crate::runner::host::HostApi;
use crate::runner::{Capabilities, RunnerError, RunnerKind, TickOutcome};

use super::bridge::HostBridge;
use super::process::ProcessTransport;
use super::transport::{ControlTransport, LoopbackTransport};
use super::{ControlRequest, ControlResponse, TransportError};

/// How a [`NativeExecutionUnit`] obtains its control transport at `start`.
#[derive(Debug, Clone)]
pub enum TransportSpec {
    /// Deterministic in-process child (tests; protocol + fault proofs).
    Loopback {
        die_after: Option<u32>,
        fail_start: bool,
    },
    /// A real OS subprocess.
    Process { program: String, args: Vec<String> },
}

impl TransportSpec {
    fn open(&self) -> Result<Box<dyn ControlTransport>, TransportError> {
        match self {
            TransportSpec::Loopback {
                die_after,
                fail_start,
            } => {
                let mut t = LoopbackTransport::new();
                if let Some(n) = die_after {
                    t = t.dying_after(*n);
                }
                if *fail_start {
                    t = t.failing_start();
                }
                Ok(Box::new(t))
            }
            TransportSpec::Process { program, args } => {
                Ok(Box::new(ProcessTransport::spawn(program, args)?))
            }
        }
    }
}

/// The execution unit for a native runner. Holds the spec + declared signal names
/// + (after `start`) the live control transport. No node, no host data.
pub struct NativeExecutionUnit {
    spec: TransportSpec,
    publish_names: Vec<String>,
    consume_names: Vec<String>,
    caps: Capabilities,
    transport: Option<Box<dyn ControlTransport>>,
    /// Set true only after a successful Load+Bind+Start handshake at `start`.
    started_ok: bool,
}

impl NativeExecutionUnit {
    pub fn new(
        spec: TransportSpec,
        publish_names: Vec<String>,
        consume_names: Vec<String>,
        caps: Capabilities,
    ) -> Self {
        Self {
            spec,
            publish_names,
            consume_names,
            caps,
            transport: None,
            started_ok: false,
        }
    }
}

impl ExecutionUnit for NativeExecutionUnit {
    fn publishes(&self) -> &[String] {
        &self.publish_names
    }
    fn consumes(&self) -> &[String] {
        &self.consume_names
    }
    fn caps(&self) -> Capabilities {
        self.caps
    }

    fn bind(&mut self, host: &mut dyn HostApi) {
        // The runner forwards to the host through the bridge (subscribe → host).
        // Results are discarded — the typestate owns the real handshake; the unit
        // keeps no shadow state.
        let mut bridge = HostBridge::new(host);
        for name in &self.publish_names {
            let _ = bridge.subscribe(name);
        }
    }

    fn start(&mut self, _host: &mut dyn HostApi) {
        // Heavy init: spawn the child, then run its control handshake. The child
        // is born here (not at load), so Load/Bind/Start cluster at start().
        let mut transport = match self.spec.open() {
            Ok(t) => t,
            Err(_) => {
                self.started_ok = false;
                return; // start failure → first tick reports Faulted (Step 2.5)
            }
        };
        let ok = matches!(transport.request(ControlRequest::Load), Ok(ControlResponse::Loaded))
            && matches!(transport.request(ControlRequest::Bind), Ok(ControlResponse::Bound))
            && matches!(transport.request(ControlRequest::Start), Ok(ControlResponse::Started));
        if ok {
            self.transport = Some(transport);
            self.started_ok = true;
        } else {
            self.started_ok = false; // transport dropped here → child reaped
        }
    }

    fn run(&mut self, _host: &mut dyn HostApi, timing: Timing) -> TickOutcome {
        if !self.started_ok {
            return TickOutcome::Faulted("native start failed".into());
        }
        let Some(transport) = self.transport.as_mut() else {
            return TickOutcome::Faulted("no transport".into());
        };
        // One budgeted control round-trip; a transport error is a Fault.
        match transport.request(ControlRequest::tick(timing)) {
            Ok(ControlResponse::Ticked(outcome)) => outcome,
            Ok(_) => TickOutcome::Faulted("unexpected control response".into()),
            Err(e) => {
                self.started_ok = false; // latch the fault for the supervisor
                TickOutcome::Faulted(format!("transport: {e:?}"))
            }
        }
    }

    fn stop(&mut self, _host: &mut dyn HostApi) {
        if let Some(mut transport) = self.transport.take() {
            // Graceful intent, then forced reap via Drop.
            let _ = transport.request(ControlRequest::Stop);
            let _ = transport.request(ControlRequest::Unload);
        }
        self.started_ok = false;
    }
}

/// A `RunnerBackend` that runs its work in a subprocess (or a loopback child).
/// `load` builds the unit only — no process is spawned until `start`.
pub struct NativeRunnerBackend {
    spec: TransportSpec,
    publish_names: Vec<String>,
    consume_names: Vec<String>,
    caps: Capabilities,
}

impl NativeRunnerBackend {
    pub fn new(spec: TransportSpec, publish_names: Vec<String>, caps: Capabilities) -> Self {
        Self {
            spec,
            publish_names,
            consume_names: Vec::new(),
            caps,
        }
    }

    /// Deterministic loopback child (tests).
    pub fn loopback(publish_names: &[&str]) -> Self {
        Self::new(
            TransportSpec::Loopback {
                die_after: None,
                fail_start: false,
            },
            publish_names.iter().map(|s| s.to_string()).collect(),
            Capabilities::default(),
        )
    }
    /// Loopback child that crashes after `n` ticks.
    pub fn loopback_dying(publish_names: &[&str], n: u32) -> Self {
        Self::new(
            TransportSpec::Loopback {
                die_after: Some(n),
                fail_start: false,
            },
            publish_names.iter().map(|s| s.to_string()).collect(),
            Capabilities::default(),
        )
    }
    /// Loopback child that fails to start.
    pub fn loopback_failing_start(publish_names: &[&str]) -> Self {
        Self::new(
            TransportSpec::Loopback {
                die_after: None,
                fail_start: true,
            },
            publish_names.iter().map(|s| s.to_string()).collect(),
            Capabilities::default(),
        )
    }
    /// A real OS subprocess.
    pub fn process(program: &str, args: &[&str], publish_names: &[&str]) -> Self {
        Self::new(
            TransportSpec::Process {
                program: program.to_string(),
                args: args.iter().map(|s| s.to_string()).collect(),
            },
            publish_names.iter().map(|s| s.to_string()).collect(),
            Capabilities::default(),
        )
    }
}

impl RunnerBackend for NativeRunnerBackend {
    fn kind(&self) -> RunnerKind {
        RunnerKind::Native
    }

    fn load(self: Box<Self>) -> Result<LoadedRunner, RunnerError> {
        // Metadata only — no spawn, no heavy init.
        let unit = NativeExecutionUnit::new(self.spec, self.publish_names, self.consume_names, self.caps);
        Ok(LoadedRunner::new(RunnerKind::Native, Box::new(unit)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::host::NullHost;

    fn timing() -> Timing {
        Timing { dt: 0.0, elapsed: 0.0 }
    }

    #[test]
    fn native_lifecycle_over_loopback() {
        let backend: Box<dyn RunnerBackend> = Box::new(NativeRunnerBackend::loopback(&["x"]));
        assert_eq!(backend.kind(), RunnerKind::Native);

        let mut host = NullHost;
        let loaded = backend.load().unwrap(); // metadata only — nothing spawned
        let bound = loaded.bind(&mut host);
        assert_eq!(bound.handshake().version, crate::runner::ABI_VERSION);
        let mut running = bound.start(&mut host); // spawns + handshakes the child
        assert_eq!(running.tick(&mut host, timing()), TickOutcome::Ok);
        assert_eq!(running.tick(&mut host, timing()), TickOutcome::Ok);
        let _stopped = running.stop(&mut host); // graceful + reap
    }

    #[test]
    fn start_failure_surfaces_as_faulted_tick() {
        let backend: Box<dyn RunnerBackend> = Box::new(NativeRunnerBackend::loopback_failing_start(&["x"]));
        let mut host = NullHost;
        let mut running = backend.load().unwrap().bind(&mut host).start(&mut host);
        // start() is infallible (typestate); failure shows on the first tick.
        assert!(matches!(running.tick(&mut host, timing()), TickOutcome::Faulted(_)));
    }

    #[test]
    fn crash_midrun_faults_then_latches() {
        let backend: Box<dyn RunnerBackend> = Box::new(NativeRunnerBackend::loopback_dying(&["x"], 1));
        let mut host = NullHost;
        let mut running = backend.load().unwrap().bind(&mut host).start(&mut host);
        assert_eq!(running.tick(&mut host, timing()), TickOutcome::Ok); // tick 1
        assert!(matches!(running.tick(&mut host, timing()), TickOutcome::Faulted(_))); // tick 2 crashes
        assert!(matches!(running.tick(&mut host, timing()), TickOutcome::Faulted(_)), "fault latches");
        let _ = running.stop(&mut host);
    }

    /// Real subprocess: spawn → start handshake (synthetic) → tick Ok → stop+reap.
    #[cfg(unix)]
    #[test]
    fn native_lifecycle_over_real_process() {
        let backend: Box<dyn RunnerBackend> = Box::new(NativeRunnerBackend::process(
            "sh",
            &["-c", "cat >/dev/null"],
            &["x"],
        ));
        let mut host = NullHost;
        let mut running = backend.load().unwrap().bind(&mut host).start(&mut host);
        assert_eq!(running.tick(&mut host, timing()), TickOutcome::Ok);
        let _stopped = running.stop(&mut host); // reaps the child
    }
}
