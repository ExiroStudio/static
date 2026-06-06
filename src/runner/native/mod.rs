//! NativeRunner — subprocess transport realization (Phase 3b, Step 3).
//!
//! Realizes the **control plane** (Step 2.5) over an out-of-process child, behind
//! the frozen `RunnerBackend`/`ExecutionUnit` seam. Nothing here changes the
//! typestate, `HostApi`, `Supervisor`, `Handshake`, `TickOutcome`, or
//! `SignalStore`; the runner is still **dormant** (the live engine never
//! references it).
//!
//! ```text
//!   NativeRunnerBackend ─load─▶ LoadedRunner (NativeExecutionUnit)
//!        start ─▶ spawn NativeProcess ─▶ ControlTransport (Load/Bind/Start)
//!        tick  ─▶ ControlTransport.request(Tick) ─▶ TickOutcome
//!        stop  ─▶ Stop/Unload, terminate + reap
//!   ProcessSupervisor: frozen Supervisor + respawn glue (crash → Fault → restart)
//!   HostBridge: forwarding HostApi proxy (runner forwards; never owns data)
//! ```
//!
//! **Boundary rule (frozen):** only the eight control messages cross the
//! transport. The data plane (signals/frames/resources/metrics) never enters the
//! transport — `HostApi` remains the authority; the bridge forwards.

mod backend;
mod bridge;
mod process;
mod supervisor;
mod transport;

pub use backend::{NativeExecutionUnit, NativeRunnerBackend, TransportSpec};
pub use bridge::HostBridge;
pub use process::{NativeProcess, ProcessTransport};
pub use supervisor::{ProcessSupervisor, SupervisionReport};
pub use transport::{ControlTransport, LoopbackTransport};

use crate::behavior::node::Timing;
use crate::runner::TickOutcome;

/// The eight control messages, host→runner request side. Mirrors the frozen
/// Step-2.5 set; carries **control only** (no signals/frames/resources). `Tick`
/// carries `Timing` (the only payload).
#[derive(Debug, Clone, PartialEq)]
pub enum ControlRequest {
    Load,
    Bind,
    Start,
    Tick { dt: f32, elapsed: f32 },
    Stop,
    Unload,
}

impl ControlRequest {
    pub fn tick(t: Timing) -> Self {
        ControlRequest::Tick {
            dt: t.dt,
            elapsed: t.elapsed,
        }
    }
}

/// The runner→host response side. A successful response is an implicit
/// Heartbeat; a transport error is a Fault (Step 2.5) — neither is a separate
/// streamed message, keeping the transport strictly request→response.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlResponse {
    Loaded,
    Bound,
    Started,
    Ticked(TickOutcome),
    Stopped,
    Unloaded,
}

/// Why a control round-trip failed. All map to a Fault → `Supervisor::on_fault`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// Child gone / channel closed.
    Closed,
    /// No response within the tick budget.
    Timeout,
    /// A response that violates the protocol/legality.
    Protocol(&'static str),
    /// Underlying IO failure.
    Io(String),
}

// ---- protocol adapter: control-only byte codec -----------------------------
//
// Realizes the frozen message *shape* as bytes for a transport. Control only —
// there is no signal/frame/resource encoding here, by rule. Tiny tag + fixed
// fields; round-trip tested.

pub(crate) fn encode_request(req: &ControlRequest) -> Vec<u8> {
    let mut out = Vec::with_capacity(9);
    match req {
        ControlRequest::Load => out.push(0),
        ControlRequest::Bind => out.push(1),
        ControlRequest::Start => out.push(2),
        ControlRequest::Tick { dt, elapsed } => {
            out.push(3);
            out.extend_from_slice(&dt.to_le_bytes());
            out.extend_from_slice(&elapsed.to_le_bytes());
        }
        ControlRequest::Stop => out.push(4),
        ControlRequest::Unload => out.push(5),
    }
    out
}

pub(crate) fn decode_request(bytes: &[u8]) -> Result<ControlRequest, TransportError> {
    match bytes.first() {
        Some(0) => Ok(ControlRequest::Load),
        Some(1) => Ok(ControlRequest::Bind),
        Some(2) => Ok(ControlRequest::Start),
        Some(3) if bytes.len() >= 9 => {
            let dt = f32::from_le_bytes(bytes[1..5].try_into().unwrap());
            let elapsed = f32::from_le_bytes(bytes[5..9].try_into().unwrap());
            Ok(ControlRequest::Tick { dt, elapsed })
        }
        Some(4) => Ok(ControlRequest::Stop),
        Some(5) => Ok(ControlRequest::Unload),
        _ => Err(TransportError::Protocol("bad request frame")),
    }
}

pub(crate) fn encode_response(resp: &ControlResponse) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    match resp {
        ControlResponse::Loaded => out.push(0),
        ControlResponse::Bound => out.push(1),
        ControlResponse::Started => out.push(2),
        ControlResponse::Ticked(o) => {
            out.push(3);
            encode_outcome(o, &mut out);
        }
        ControlResponse::Stopped => out.push(4),
        ControlResponse::Unloaded => out.push(5),
    }
    out
}

pub(crate) fn decode_response(bytes: &[u8]) -> Result<ControlResponse, TransportError> {
    match bytes.first() {
        Some(0) => Ok(ControlResponse::Loaded),
        Some(1) => Ok(ControlResponse::Bound),
        Some(2) => Ok(ControlResponse::Started),
        Some(3) => Ok(ControlResponse::Ticked(decode_outcome(&bytes[1..])?)),
        Some(4) => Ok(ControlResponse::Stopped),
        Some(5) => Ok(ControlResponse::Unloaded),
        _ => Err(TransportError::Protocol("bad response frame")),
    }
}

fn encode_outcome(o: &TickOutcome, out: &mut Vec<u8>) {
    match o {
        TickOutcome::Ok => out.push(0),
        TickOutcome::Idle => out.push(1),
        TickOutcome::Faulted(reason) => {
            out.push(2);
            let bytes = reason.as_bytes();
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(bytes);
        }
    }
}

fn decode_outcome(bytes: &[u8]) -> Result<TickOutcome, TransportError> {
    match bytes.first() {
        Some(0) => Ok(TickOutcome::Ok),
        Some(1) => Ok(TickOutcome::Idle),
        Some(2) if bytes.len() >= 5 => {
            let len = u32::from_le_bytes(bytes[1..5].try_into().unwrap()) as usize;
            let reason = String::from_utf8_lossy(&bytes[5..(5 + len).min(bytes.len())]).into_owned();
            Ok(TickOutcome::Faulted(reason))
        }
        _ => Err(TransportError::Protocol("bad outcome frame")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_codec_round_trips() {
        for req in [
            ControlRequest::Load,
            ControlRequest::Bind,
            ControlRequest::Start,
            ControlRequest::Tick { dt: 0.033, elapsed: 1.5 },
            ControlRequest::Stop,
            ControlRequest::Unload,
        ] {
            assert_eq!(decode_request(&encode_request(&req)).unwrap(), req);
        }
    }

    #[test]
    fn response_codec_round_trips_including_fault_reason() {
        for resp in [
            ControlResponse::Loaded,
            ControlResponse::Bound,
            ControlResponse::Started,
            ControlResponse::Ticked(TickOutcome::Ok),
            ControlResponse::Ticked(TickOutcome::Idle),
            ControlResponse::Ticked(TickOutcome::Faulted("boom".into())),
            ControlResponse::Stopped,
            ControlResponse::Unloaded,
        ] {
            assert_eq!(decode_response(&encode_response(&resp)).unwrap(), resp);
        }
    }

    #[test]
    fn malformed_frames_are_protocol_errors_not_panics() {
        assert!(decode_request(&[]).is_err());
        assert!(decode_request(&[99]).is_err());
        assert!(decode_request(&[3, 0, 0]).is_err()); // Tick missing timing
        assert!(decode_response(&[99]).is_err());
    }
}
