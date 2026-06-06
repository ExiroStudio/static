//! [`NativeProcess`] — a single real child process, and [`ProcessTransport`] —
//! the control channel over its stdio.
//!
//! Process rules (frozen): **one runner, one process** — no nesting, no daemon,
//! no worker pool. The process is spawned at `start` (heavy init), written to via
//! stdin, watched for exit (→ Fault), and force-reaped on drop (no zombie, no
//! panic escape).
//!
//! **Step-3 honesty:** there is no protocol-speaking child yet (an addon binary
//! is out of scope — no SDK/packaging). So `ProcessTransport` proves the *real*
//! parts — spawn, write-to-child, liveness, crash detection, reap — and
//! **synthesizes** the control response from liveness. Parsing a child's real
//! protocol reply needs that child to exist (Known Debt → Step 4). The full
//! protocol round-trip is proven deterministically by [`LoopbackTransport`].

use std::io::Write;
use std::process::{Child, Command, Stdio};

use super::transport::ControlTransport;
use super::{encode_request, ControlRequest, ControlResponse, TransportError};

/// Owns exactly one OS child process. Spawn → write → liveness → terminate/reap.
pub struct NativeProcess {
    child: Child,
}

impl NativeProcess {
    /// Spawn `program args…` with a piped stdin (to write control frames) and
    /// discarded stdout/stderr (Step 3 reads no child output — see module note).
    pub fn spawn(program: &str, args: &[String]) -> Result<Self, TransportError> {
        let child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| TransportError::Io(e.to_string()))?;
        Ok(Self { child })
    }

    /// Non-blocking liveness: `try_wait` returns `Ok(None)` while running.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Write a length-prefixed control frame to the child's stdin. A dead child
    /// yields `BrokenPipe` → `Closed` (never hangs).
    pub fn write_frame(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or(TransportError::Closed)?;
        let len = (bytes.len() as u32).to_le_bytes();
        stdin
            .write_all(&len)
            .and_then(|_| stdin.write_all(bytes))
            .and_then(|_| stdin.flush())
            .map_err(|_| TransportError::Closed)
    }

    /// Best-effort graceful-then-forced termination + reap. (`std::process` only
    /// exposes SIGKILL on unix; a SIGTERM grace window needs `libc` — Known Debt.)
    pub fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for NativeProcess {
    fn drop(&mut self) {
        // Reap unconditionally — no zombie, no escape.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Control transport over a real child's stdio. See module note on synthesized
/// responses.
pub struct ProcessTransport {
    process: NativeProcess,
}

impl ProcessTransport {
    pub fn spawn(program: &str, args: &[String]) -> Result<Self, TransportError> {
        Ok(Self {
            process: NativeProcess::spawn(program, args)?,
        })
    }

    /// The response the child *would* send for `req` (synthesized from liveness in
    /// Step 3 — see module note).
    fn synthetic_ack(req: &ControlRequest) -> ControlResponse {
        match req {
            ControlRequest::Load => ControlResponse::Loaded,
            ControlRequest::Bind => ControlResponse::Bound,
            ControlRequest::Start => ControlResponse::Started,
            ControlRequest::Tick { .. } => ControlResponse::Ticked(crate::runner::TickOutcome::Ok),
            ControlRequest::Stop => ControlResponse::Stopped,
            ControlRequest::Unload => ControlResponse::Unloaded,
        }
    }
}

impl ControlTransport for ProcessTransport {
    fn request(&mut self, req: ControlRequest) -> Result<ControlResponse, TransportError> {
        if !self.process.is_alive() {
            return Err(TransportError::Closed); // crash → Fault
        }
        // Real IO: the control frame is written to the actual child.
        self.process.write_frame(&encode_request(&req))?;
        // Re-check liveness after the write (the child may have exited).
        if !self.process.is_alive() {
            return Err(TransportError::Closed);
        }
        Ok(Self::synthetic_ack(&req))
    }

    fn is_alive(&mut self) -> bool {
        self.process.is_alive()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str) -> Vec<String> {
        vec!["-c".to_string(), script.to_string()]
    }

    /// Spawn a real long-lived child, confirm liveness, then terminate + reap.
    #[test]
    fn spawns_a_real_process_and_reaps_it() {
        let mut p = NativeProcess::spawn("sh", &sh("sleep 30")).expect("spawn");
        assert!(p.is_alive(), "child should be running");
        p.terminate();
        assert!(!p.is_alive(), "child must be reaped after terminate");
    }

    /// A child that exits is detected as dead → control request returns Closed
    /// (→ Fault). Proves real crash detection.
    #[test]
    fn dead_child_surfaces_as_closed() {
        let mut t = ProcessTransport::spawn("sh", &sh("exit 1")).expect("spawn");
        // Bounded wait for the child to exit.
        let mut waited = 0;
        while t.is_alive() && waited < 200 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            waited += 1;
        }
        assert!(!t.is_alive(), "child exited");
        assert_eq!(t.request(ControlRequest::Tick { dt: 0.0, elapsed: 0.0 }), Err(TransportError::Closed));
    }

    /// A live child that consumes stdin accepts a written control frame and the
    /// transport synthesizes the ack (Step-3 behavior).
    #[test]
    fn live_child_accepts_a_control_frame() {
        let mut t = ProcessTransport::spawn("sh", &sh("cat >/dev/null")).expect("spawn");
        assert!(t.is_alive());
        assert_eq!(t.request(ControlRequest::Load), Ok(ControlResponse::Loaded));
        // Dropping the transport reaps the child (Drop on NativeProcess).
    }
}
