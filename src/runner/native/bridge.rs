//! [`HostBridge`] — the forwarding `HostApi` proxy handed across the runner seam.
//!
//! The runner **forwards** every data-plane call to the real host and owns
//! nothing: no signals, frames, resources, or metrics; no cache, no shadow state,
//! no reinterpretation. Its behavior is byte-for-byte that of the real host
//! (`publish() → host`, `subscribe() → host`, `read_frame() → host`, …). This is
//! the object a future data-plane transport would place at the child boundary;
//! today it forwards in-process (the data plane does not cross the control
//! transport — frozen).

use crate::addon::schema::ParamValue;
use crate::behavior::node::Timing;
use crate::runner::host::{
    CapabilityDenied, FrameRef, FrameTier, HostApi, LogLevel, Metrics, ResourceHandle, WorkerHandle,
};
use crate::signal::{SignalId, SignalValue};

/// A transparent forwarder over a borrowed real host. `'h` ties it to the host
/// for the call/tick — the bridge never outlives or retains host state.
pub struct HostBridge<'h> {
    inner: &'h mut dyn HostApi,
}

impl<'h> HostBridge<'h> {
    pub fn new(inner: &'h mut dyn HostApi) -> Self {
        Self { inner }
    }
}

impl HostApi for HostBridge<'_> {
    fn publish(&mut self, id: SignalId, value: SignalValue) {
        self.inner.publish(id, value); // → host (no buffering)
    }
    fn subscribe(&mut self, name: &str) -> Option<SignalId> {
        self.inner.subscribe(name) // → host
    }
    fn request_frame(&mut self, tier: FrameTier) -> bool {
        self.inner.request_frame(tier)
    }
    fn change_frame_tier(&mut self, tier: FrameTier) -> bool {
        self.inner.change_frame_tier(tier)
    }
    fn read_frame(&mut self) -> Option<FrameRef> {
        self.inner.read_frame() // → host (borrow; no cache)
    }
    fn get_param(&self, key: &str) -> Option<ParamValue> {
        self.inner.get_param(key)
    }
    fn timing(&self) -> Timing {
        self.inner.timing()
    }
    fn log(&self, level: LogLevel, message: &str) {
        self.inner.log(level, message)
    }
    fn request_resource(&mut self, id: &str) -> Option<ResourceHandle> {
        self.inner.request_resource(id) // host-owned handle; runner never owns it
    }
    fn release_resource(&mut self, handle: ResourceHandle) {
        self.inner.release_resource(handle)
    }
    fn spawn_worker(&mut self) -> Result<WorkerHandle, CapabilityDenied> {
        self.inner.spawn_worker()
    }
    fn heartbeat(&mut self) {
        self.inner.heartbeat()
    }
    fn metrics(&self) -> Metrics {
        self.inner.metrics()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::host::RecordingHost;
    use crate::signal::{SignalKind, SignalSchema};
    use std::sync::Arc;

    #[test]
    fn forwards_publish_and_subscribe_to_the_real_host() {
        let schema = Arc::new(SignalSchema::from_pairs(&[("x", SignalKind::F32)]));
        let mut host = RecordingHost::with_schema(schema);
        {
            let mut bridge = HostBridge::new(&mut host);
            let id = bridge.subscribe("x").expect("resolved via the real host");
            bridge.publish(id, SignalValue::F32(2.0));
            bridge.heartbeat();
            assert!(bridge.request_frame(FrameTier::R160x90));
        }
        // The real host saw everything — the bridge kept nothing.
        assert_eq!(host.published, vec![(0, SignalValue::F32(2.0))]);
        assert_eq!(host.heartbeats, 1);
        assert_eq!(host.frame_requests, 1);
        assert_eq!(host.subscribed_tier, Some(FrameTier::R160x90));
    }

    #[test]
    fn forwarding_is_identical_to_calling_the_host_directly() {
        // subscribe of an unknown name forwards the host's own answer (None).
        let mut host = RecordingHost::new();
        let mut bridge = HostBridge::new(&mut host);
        assert_eq!(bridge.subscribe("missing"), None);
        assert!(bridge.read_frame().is_none());
        assert_eq!(bridge.metrics(), Metrics::default());
        assert!(bridge.spawn_worker().is_err(), "deny-by-default forwarded");
    }
}
