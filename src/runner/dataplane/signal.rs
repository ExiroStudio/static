//! [`SignalBridge`] — realizes `publish`/`subscribe` over a host-owned
//! `SignalStore`, preserving the frozen Step-2.6 semantics.
//!
//! * `publish` — **overwrite** (same id within a tick → last wins), no queue, no
//!   stream, no event bus.
//! * `commit` — **one atomic** buffer swap per tick (latest-wins across ticks).
//! * `subscribe` — **snapshot** name→id resolution, session-stable. No callback,
//!   no listener.
//!
//! It *uses* `SignalStore` (it does not modify it). The store is host-side; only
//! POD (`SignalId`/`SignalValue`) ever leaves through the `HostApi`.

use std::sync::Arc;

use crate::signal::{
    SignalId, SignalPublisher, SignalReader, SignalSchema, SignalSnapshot, SignalStore, SignalValue,
};

pub struct SignalBridge {
    schema: Arc<SignalSchema>,
    publisher: SignalPublisher,
    reader: SignalReader,
    snap: SignalSnapshot,
}

impl SignalBridge {
    pub fn new(schema: Arc<SignalSchema>) -> Self {
        let (publisher, reader) = SignalStore::new(&schema);
        let snap = reader.snapshot();
        Self {
            schema,
            publisher,
            reader,
            snap,
        }
    }

    /// Snapshot resolution — a hashed lookup, stable for the session. No listener.
    pub fn subscribe(&self, name: &str) -> Option<SignalId> {
        self.schema.id(name)
    }

    /// Stage a value (overwrite). Repeated `publish` to the same id before
    /// `commit` keeps only the last.
    pub fn publish(&mut self, id: SignalId, value: SignalValue) {
        self.publisher.set(id, value);
    }

    /// Commit the staged frame as one atomic swap (latest-wins handoff).
    pub fn commit(&mut self) {
        self.publisher.publish();
    }

    /// Refresh and return the consumer's latest snapshot (latest-wins). Used by
    /// the render/consumer side; pinned per tick by [`BridgedHost`](super::BridgedHost).
    pub fn read_latest(&mut self) -> &SignalSnapshot {
        self.reader.snapshot_into(&mut self.snap);
        &self.snap
    }

    /// The currently-pinned snapshot without refreshing (tick-consistent reads).
    pub fn snapshot(&self) -> &SignalSnapshot {
        &self.snap
    }

    /// Re-pin the snapshot at tick start (snapshot semantics).
    pub fn pin(&mut self) {
        self.reader.snapshot_into(&mut self.snap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::SignalKind;

    fn schema() -> Arc<SignalSchema> {
        Arc::new(SignalSchema::from_pairs(&[
            ("a", SignalKind::F32),
            ("b", SignalKind::Vec2),
        ]))
    }

    #[test]
    fn publish_overwrites_within_a_tick_last_wins() {
        let mut s = SignalBridge::new(schema());
        let a = s.subscribe("a").unwrap();
        s.publish(a, SignalValue::F32(1.0));
        s.publish(a, SignalValue::F32(2.0)); // overwrite
        s.publish(a, SignalValue::F32(3.0)); // overwrite
        s.commit();
        assert_eq!(s.read_latest().get(a).as_f32(), Some(3.0), "last write wins");
    }

    #[test]
    fn commit_is_latest_wins_across_ticks() {
        let mut s = SignalBridge::new(schema());
        let a = s.subscribe("a").unwrap();
        s.publish(a, SignalValue::F32(10.0));
        s.commit();
        s.publish(a, SignalValue::F32(20.0));
        s.commit();
        assert_eq!(s.read_latest().get(a).as_f32(), Some(20.0), "newest committed frame");
    }

    #[test]
    fn subscribe_is_a_stable_snapshot_lookup() {
        let s = SignalBridge::new(schema());
        assert_eq!(s.subscribe("a").unwrap().index(), 0);
        assert_eq!(s.subscribe("b").unwrap().index(), 1);
        assert_eq!(s.subscribe("a").unwrap().index(), 0, "stable across calls");
        assert!(s.subscribe("missing").is_none());
    }

    #[test]
    fn pinned_snapshot_is_consistent_until_repinned() {
        let mut s = SignalBridge::new(schema());
        let a = s.subscribe("a").unwrap();
        s.publish(a, SignalValue::F32(1.0));
        s.commit();
        s.pin();
        let first = s.snapshot().get(a).as_f32();
        // A new commit must NOT change the pinned snapshot until re-pinned.
        s.publish(a, SignalValue::F32(9.0));
        s.commit();
        assert_eq!(s.snapshot().get(a).as_f32(), first, "pinned reads stay consistent");
        s.pin();
        assert_eq!(s.snapshot().get(a).as_f32(), Some(9.0), "re-pin samples latest");
    }
}
