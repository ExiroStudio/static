//! Data-plane realization (Phase 3b, Step 4).
//!
//! Realizes the frozen `HostApi` data-plane *behavior* (Step 2.6) as five
//! host-side bridges, composed into a real [`BridgedHost`] (a `HostApi`):
//!
//! | Bridge | Method(s) | Frozen rule preserved |
//! |--------|-----------|------------------------|
//! | [`SignalBridge`]   | publish / subscribe          | overwrite · latest-wins · single commit · snapshot |
//! | [`FrameBridge`]    | request_frame / change_frame_tier / read_frame | borrow · tick-pinned · latest · no copy/GPU |
//! | [`ResourceBridge`] | request_resource / release_resource | opaque handle · lazy/cache · idempotent release |
//! | [`MetricsBridge`]  | metrics                      | addon-local snapshot only |
//! | [`WorkerBridge`]   | spawn_worker                 | runner-owned · host tracks · depth = 1 |
//!
//! **Snapshot semantics:** [`BridgedHost::begin_tick`] pins the signal snapshot
//! and the frame for the whole tick, so repeated reads are consistent; the next
//! tick samples latest-wins. Only POD/ids/opaque-handles ever leave through the
//! `HostApi` (the bridges hold the non-POD host-side state). **Not wired into the
//! engine** — a standalone, dormant realization used by tests.

mod frame;
mod metrics;
mod resource;
mod signal;
mod worker;

pub use frame::FrameBridge;
pub use metrics::MetricsBridge;
pub use resource::ResourceBridge;
pub use signal::SignalBridge;
pub use worker::WorkerBridge;

use std::sync::Arc;

use crate::addon::schema::{ParamMap, ParamValue};
use crate::behavior::node::Timing;
use crate::runner::host::{
    CapabilityDenied, FrameRef, FrameTier, HostApi, LogLevel, Metrics, ResourceHandle, WorkerHandle,
};
use crate::signal::{SignalId, SignalSchema, SignalSnapshot, SignalValue};

/// A real `HostApi` composed of the five data-plane bridges. The host-side
/// authority for one runner; the runner reaches it through the `HostBridge`
/// forwarder. Holds all data; the runner owns none.
pub struct BridgedHost {
    signals: SignalBridge,
    frames: FrameBridge,
    resources: ResourceBridge,
    metrics: MetricsBridge,
    workers: WorkerBridge,
    params: ParamMap,
    timing: Timing,
    heartbeats: u32,
}

impl BridgedHost {
    pub fn new(schema: Arc<SignalSchema>, worker_capability: bool) -> Self {
        Self {
            signals: SignalBridge::new(schema),
            frames: FrameBridge::new(),
            resources: ResourceBridge::new(),
            metrics: MetricsBridge::new(),
            workers: WorkerBridge::new(worker_capability),
            params: ParamMap::new(),
            timing: Timing { dt: 0.0, elapsed: 0.0 },
            heartbeats: 0,
        }
    }

    pub fn with_param(mut self, key: &str, value: ParamValue) -> Self {
        self.params.insert(key.to_string(), value);
        self
    }

    // ---- tick / snapshot discipline ----

    /// Pin the signal snapshot + frame for this tick and set timing. Reads during
    /// the tick are consistent; the next `begin_tick` samples latest-wins.
    pub fn begin_tick(&mut self, timing: Timing) {
        self.timing = timing;
        self.signals.pin();
        self.frames.begin_tick();
    }

    /// Commit staged publishes — one atomic frame swap.
    pub fn commit(&mut self) {
        self.signals.commit();
    }

    /// Consumer-side latest signal snapshot (render/filter side).
    pub fn read_latest_signals(&mut self) -> &SignalSnapshot {
        self.signals.read_latest()
    }

    // ---- host-side feeds + diagnostics (used by the supervisor/tests) ----

    pub fn push_frame(&mut self, width: u32, height: u32) {
        self.frames.push(width, height);
    }
    pub fn set_metrics(&mut self, metrics: Metrics) {
        self.metrics.update(metrics);
    }
    pub fn invalidate_resources(&mut self) {
        self.resources.invalidate();
    }
    pub fn resource_refcount(&self, id: &str) -> u32 {
        self.resources.refcount(id)
    }
    pub fn live_workers(&self) -> u32 {
        self.workers.live()
    }
    pub fn heartbeats(&self) -> u32 {
        self.heartbeats
    }
}

impl HostApi for BridgedHost {
    fn publish(&mut self, id: SignalId, value: SignalValue) {
        self.signals.publish(id, value);
    }
    fn subscribe(&mut self, name: &str) -> Option<SignalId> {
        self.signals.subscribe(name)
    }
    fn request_frame(&mut self, tier: FrameTier) -> bool {
        self.frames.request_frame(tier)
    }
    fn change_frame_tier(&mut self, tier: FrameTier) -> bool {
        self.frames.change_frame_tier(tier)
    }
    fn read_frame(&mut self) -> Option<FrameRef> {
        self.frames.read_frame()
    }
    fn get_param(&self, key: &str) -> Option<ParamValue> {
        self.params.get(key).cloned()
    }
    fn timing(&self) -> Timing {
        self.timing
    }
    fn log(&self, _level: LogLevel, _message: &str) {
        // sink (data-plane log is fire-and-forget; not retained)
    }
    fn request_resource(&mut self, id: &str) -> Option<ResourceHandle> {
        self.resources.request(id)
    }
    fn release_resource(&mut self, handle: ResourceHandle) {
        self.resources.release(handle);
    }
    fn spawn_worker(&mut self) -> Result<WorkerHandle, CapabilityDenied> {
        self.workers.spawn()
    }
    fn heartbeat(&mut self) {
        self.heartbeats += 1;
    }
    fn metrics(&self) -> Metrics {
        self.metrics.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::builtins::time;
    use crate::runner::backend::RunnerBackend;
    use crate::runner::{InProcessRustRunner, NativeRunnerBackend};
    use crate::signal::SignalKind;

    fn time_schema() -> Arc<SignalSchema> {
        Arc::new(SignalSchema::from_pairs(&[("signal.time", SignalKind::F32)]))
    }

    /// End-to-end: a real behavior, driven through a runner, reaches the data
    /// plane via the `HostApi` and its output is observable on the host snapshot.
    /// runner → HostApi(BridgedHost) → SignalBridge → commit → snapshot.
    #[test]
    fn behavior_publishes_through_the_data_plane() {
        let mut host = BridgedHost::new(time_schema(), false);
        let backend: Box<dyn RunnerBackend> =
            Box::new(InProcessRustRunner::new(time::init_with, "b", ParamMap::new(), true));
        let t = Timing { dt: 0.0, elapsed: std::f32::consts::FRAC_PI_2 };

        let mut running = backend.load().unwrap().bind(&mut host).start(&mut host);
        host.begin_tick(t);
        running.tick(&mut host, t); // the behavior publishes signal.time = sin(π/2)
        host.commit();

        let id = host.subscribe("signal.time").unwrap();
        assert!((host.read_latest_signals().get(id).as_f32().unwrap() - 1.0).abs() < 1e-5);
        let _ = running.stop(&mut host);
    }

    /// A crashing NativeRunner does not corrupt the host data plane (isolation):
    /// after the runner faults, the bridges still publish/commit/read correctly.
    #[test]
    fn runner_crash_does_not_corrupt_the_data_plane() {
        let mut host = BridgedHost::new(time_schema(), false);
        let backend: Box<dyn RunnerBackend> =
            Box::new(NativeRunnerBackend::loopback_dying(&["signal.time"], 0));
        let t = Timing { dt: 0.0, elapsed: 0.0 };

        let mut running = backend.load().unwrap().bind(&mut host).start(&mut host);
        let outcome = running.tick(&mut host, t);
        assert!(matches!(outcome, crate::runner::TickOutcome::Faulted(_)));
        let _ = running.stop(&mut host);

        // Host data plane is intact after the crash.
        let id = host.subscribe("signal.time").unwrap();
        host.publish(id, SignalValue::F32(5.0));
        host.commit();
        assert_eq!(host.read_latest_signals().get(id).as_f32(), Some(5.0));
    }

    /// The composed host honors every data-plane rule through the `HostApi`.
    #[test]
    fn bridged_host_realizes_the_full_hostapi_surface() {
        let mut host = BridgedHost::new(time_schema(), true).with_param("k", ParamValue::F32(0.5));
        let h: &mut dyn HostApi = &mut host; // object-safe, POD in/out

        // signals
        let id = h.subscribe("signal.time").unwrap();
        h.publish(id, SignalValue::F32(1.0));
        // frames
        assert!(h.request_frame(FrameTier::R320x180));
        assert!(h.read_frame().is_none()); // none pushed/pinned yet
        // params / timing
        assert_eq!(h.get_param("k"), Some(ParamValue::F32(0.5)));
        assert_eq!(h.timing().elapsed, 0.0);
        // resources
        let r = h.request_resource("model").unwrap();
        h.release_resource(r);
        // workers (granted)
        assert!(h.spawn_worker().is_ok());
        // metrics / heartbeat
        let _ = h.metrics();
        h.heartbeat();

        assert_eq!(host.live_workers(), 1);
        assert_eq!(host.heartbeats(), 1);
    }

    /// Frame reads are tick-consistent through the HostApi (snapshot semantics).
    #[test]
    fn read_frame_is_tick_consistent_via_hostapi() {
        let mut host = BridgedHost::new(time_schema(), false);
        host.push_frame(100, 90);
        host.begin_tick(Timing { dt: 0.0, elapsed: 0.0 });
        let a = host.read_frame();
        host.push_frame(200, 90); // newer mid-tick
        let b = host.read_frame();
        assert_eq!(a, b, "pinned within the tick");
        host.begin_tick(Timing { dt: 0.0, elapsed: 0.0 });
        assert_eq!(host.read_frame().unwrap().width, 200, "latest-wins next tick");
    }

    /// Reload invalidates resource handles through the host (reload stability).
    #[test]
    fn reload_invalidates_resources_through_the_host() {
        let mut host = BridgedHost::new(time_schema(), false);
        let _ = host.request_resource("m").unwrap();
        assert_eq!(host.resource_refcount("m"), 1);
        host.invalidate_resources();
        assert_eq!(host.resource_refcount("m"), 0);
        assert!(host.request_resource("m").is_some(), "re-request works post-reload");
    }
}
