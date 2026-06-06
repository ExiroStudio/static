//! [`HostApi`] — the frozen capability surface, the *only* thing an addon ever
//! touches. Method signatures only; no real capability/transport implementation
//! (those are later steps). A [`NullHost`] no-op stub makes the surface
//! compile-checkable and lets the runner lifecycle be exercised in tests.
//!
//! Frozen method set (RFC §Q6, Revision 1): `publish`, `subscribe`,
//! `request_frame`, `change_frame_tier`, `read_frame`, `get_param`, `timing`,
//! `log`, `request_resource`, `release_resource`, `spawn_worker`, `heartbeat`,
//! `metrics`. There is deliberately no engine/device/queue/runtime access and no
//! config *set* — params flow host→addon only, and `metrics` is read-only.

use std::sync::Arc;

use crate::addon::schema::ParamValue;
use crate::behavior::node::Timing;
use crate::signal::{SignalId, SignalSchema, SignalValue};

/// Frame resolution tiers (frozen ladder, RFC §Q2). `FullRes` is gated behind a
/// system-tier capability; the rest are the standard downscales the host owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameTier {
    R160x90,
    R320x180,
    R640x360,
    /// Engine-resolution frame; system tier only.
    FullRes,
}

/// A read-only handle to the latest frame of the subscribed tier. Step 1 carries
/// only metadata — no pixel transport exists yet (that is a later step). It marks
/// the shape of what `read_frame` will hand back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRef {
    pub width: u32,
    pub height: u32,
    pub tier: FrameTier,
}

/// Opaque, epoch-scoped handle to a host-owned resource (model/asset). Never a
/// pointer; invalidated by the host across a reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceHandle(pub u32);

/// Opaque handle to a worker the addon spawned (within its own confinement).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerHandle(pub u32);

/// Returned when a capability-gated call is denied. The `&'static str` names the
/// capability that was missing (diagnostics only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityDenied {
    pub capability: &'static str,
}

/// Severity for [`HostApi::log`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Read-only per-addon metrics (RFC §Q6 Feedback 4). Host/supervisor-collected;
/// surfaced to the inspector and, to the addon, as self-throttle headroom. No
/// control, no writes, no cross-addon visibility. Mirrors the existing
/// `BehaviorStats` shape.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Metrics {
    pub cpu: f32,
    pub memory_bytes: u64,
    pub tick_us: f32,
    pub fps: f32,
    pub over_budget: u64,
}

/// The capability surface an addon sees — and the boundary it cannot cross.
/// Object-safe (`&mut dyn HostApi`) so a runner can hold it abstractly.
///
/// Every method is *stub-safe*: a conforming no-op implementation ([`NullHost`])
/// must satisfy the contract by denying/returning empty, never panicking. Real
/// capability/transport backing is a later step.
pub trait HostApi {
    /// Stage a published signal for this tick (the host commits atomically).
    fn publish(&mut self, id: SignalId, value: SignalValue);

    /// Resolve a consumed signal name to its slot id, once. `None` if undeclared
    /// or unpublished.
    fn subscribe(&mut self, name: &str) -> Option<SignalId>;

    /// Subscribe to camera frames at `tier`. Returns whether the host granted it
    /// (capability + tier check). `false` ⇒ no frames will be delivered.
    fn request_frame(&mut self, tier: FrameTier) -> bool;

    /// Switch the subscribed tier without reload (lock-free over pre-allocated
    /// tiers, RFC §Q2). Returns whether the switch was granted.
    fn change_frame_tier(&mut self, tier: FrameTier) -> bool;

    /// Sample the latest frame of the subscribed tier, if any. Latest-wins.
    fn read_frame(&mut self) -> Option<FrameRef>;

    /// Read a config parameter (host→addon only; the addon cannot set config).
    fn get_param(&self, key: &str) -> Option<ParamValue>;

    /// Per-tick timing for framerate-independent state.
    fn timing(&self) -> Timing;

    /// Emit a diagnostic line through the host's log sink.
    fn log(&self, level: LogLevel, message: &str);

    /// Request a host-owned resource (model/asset) by id; `None` if undeclared
    /// or denied. The handle is read-only and epoch-scoped.
    fn request_resource(&mut self, id: &str) -> Option<ResourceHandle>;

    /// Release a previously requested resource handle early.
    fn release_resource(&mut self, handle: ResourceHandle);

    /// Spawn an addon-confined worker. Denied unless the capability is granted.
    fn spawn_worker(&mut self) -> Result<WorkerHandle, CapabilityDenied>;

    /// Liveness signal for the supervisor's watchdog (for long async work).
    fn heartbeat(&mut self);

    /// Read this addon's own metrics (read-only).
    fn metrics(&self) -> Metrics;
}

/// A no-op, deny-by-default [`HostApi`]. Used for compile checks and to exercise
/// runner lifecycle without a real host. It is *not* a capability implementation
/// — it is the safe empty surface (everything denied/empty, nothing panics).
#[derive(Debug, Default)]
pub struct NullHost;

impl HostApi for NullHost {
    fn publish(&mut self, _id: SignalId, _value: SignalValue) {}
    fn subscribe(&mut self, _name: &str) -> Option<SignalId> {
        None
    }
    fn request_frame(&mut self, _tier: FrameTier) -> bool {
        false
    }
    fn change_frame_tier(&mut self, _tier: FrameTier) -> bool {
        false
    }
    fn read_frame(&mut self) -> Option<FrameRef> {
        None
    }
    fn get_param(&self, _key: &str) -> Option<ParamValue> {
        None
    }
    fn timing(&self) -> Timing {
        Timing {
            dt: 0.0,
            elapsed: 0.0,
        }
    }
    fn log(&self, _level: LogLevel, _message: &str) {}
    fn request_resource(&mut self, _id: &str) -> Option<ResourceHandle> {
        None
    }
    fn release_resource(&mut self, _handle: ResourceHandle) {}
    fn spawn_worker(&mut self) -> Result<WorkerHandle, CapabilityDenied> {
        Err(CapabilityDenied {
            capability: "spawn_worker",
        })
    }
    fn heartbeat(&mut self) {}
    fn metrics(&self) -> Metrics {
        Metrics::default()
    }
}

/// A recording [`HostApi`] for tests/mock flow: captures `publish`/`heartbeat`/
/// `request_frame`, resolves `subscribe` through an optional schema, and serves a
/// fixed `timing`. It is a *mock surface*, not a capability implementation — no
/// enforcement, no transport. (`NullHost` remains the deny-everything default.)
pub struct RecordingHost {
    schema: Option<Arc<SignalSchema>>,
    timing: Timing,
    /// `(slot index, value)` for every `publish`.
    pub published: Vec<(usize, SignalValue)>,
    pub heartbeats: u32,
    pub frame_requests: u32,
    pub subscribed_tier: Option<FrameTier>,
}

impl Default for RecordingHost {
    fn default() -> Self {
        // `Timing` is not `Default` (it's the frozen behavior contract), so spell
        // the zero timing out here rather than deriving.
        Self {
            schema: None,
            timing: Timing {
                dt: 0.0,
                elapsed: 0.0,
            },
            published: Vec::new(),
            heartbeats: 0,
            frame_requests: 0,
            subscribed_tier: None,
        }
    }
}

impl RecordingHost {
    pub fn new() -> Self {
        Self::default()
    }
    /// Resolve `subscribe`/handshake ids through this schema.
    pub fn with_schema(schema: Arc<SignalSchema>) -> Self {
        Self {
            schema: Some(schema),
            ..Self::default()
        }
    }
    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }
}

impl HostApi for RecordingHost {
    fn publish(&mut self, id: SignalId, value: SignalValue) {
        self.published.push((id.index(), value));
    }
    fn subscribe(&mut self, name: &str) -> Option<SignalId> {
        self.schema.as_ref().and_then(|s| s.id(name))
    }
    fn request_frame(&mut self, tier: FrameTier) -> bool {
        self.frame_requests += 1;
        self.subscribed_tier = Some(tier);
        true
    }
    fn change_frame_tier(&mut self, tier: FrameTier) -> bool {
        self.subscribed_tier = Some(tier);
        true
    }
    fn read_frame(&mut self) -> Option<FrameRef> {
        None
    }
    fn get_param(&self, _key: &str) -> Option<ParamValue> {
        None
    }
    fn timing(&self) -> Timing {
        self.timing
    }
    fn log(&self, _level: LogLevel, _message: &str) {}
    fn request_resource(&mut self, _id: &str) -> Option<ResourceHandle> {
        None
    }
    fn release_resource(&mut self, _handle: ResourceHandle) {}
    fn spawn_worker(&mut self) -> Result<WorkerHandle, CapabilityDenied> {
        Err(CapabilityDenied {
            capability: "spawn_worker",
        })
    }
    fn heartbeat(&mut self) {
        self.heartbeats += 1;
    }
    fn metrics(&self) -> Metrics {
        Metrics::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile + runtime check: `NullHost` satisfies the whole frozen surface and
    /// is usable as a `&mut dyn HostApi` (object-safe), denying by default.
    #[test]
    fn null_host_satisfies_the_frozen_surface() {
        // A real id minted via the schema (no signal internals touched).
        let schema = crate::signal::SignalSchema::from_pairs(&[("x", crate::signal::SignalKind::F32)]);
        let id = schema.id("x").unwrap();

        let mut host = NullHost;
        let h: &mut dyn HostApi = &mut host;
        h.publish(id, SignalValue::F32(1.0));
        assert!(h.subscribe("x").is_none());
        assert!(!h.request_frame(FrameTier::R320x180));
        assert!(!h.change_frame_tier(FrameTier::R640x360));
        assert!(h.read_frame().is_none());
        assert!(h.get_param("k").is_none());
        assert_eq!(h.timing().elapsed, 0.0);
        h.log(LogLevel::Info, "hello");
        assert!(h.request_resource("m").is_none());
        h.release_resource(ResourceHandle(0));
        assert_eq!(
            h.spawn_worker().unwrap_err().capability,
            "spawn_worker",
            "default-deny capability"
        );
        h.heartbeat();
        assert_eq!(h.metrics(), Metrics::default());
    }
}
