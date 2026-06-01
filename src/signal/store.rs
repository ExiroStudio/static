//! [`SignalStore`] — a lock-free single-producer / single-consumer triple
//! buffer that hands the consumer a consistent, whole-frame snapshot.
//!
//! ## Why three buffers, not two
//!
//! The producer writes a full frame, the consumer reads a full frame, and the
//! two run on different threads. With only *two* buffers the producer can lap
//! the consumer and overwrite the very buffer the consumer is mid-read on — a
//! data race (UB in Rust), even if the torn read is later discarded. Three
//! buffers make every buffer owned by exactly one party at any instant:
//!
//! - the **producer** owns `write_idx`,
//! - the **consumer** owns `read_idx`,
//! - the **shared** slot holds the third (the most-recently-published frame).
//!
//! `{write_idx, read_idx, shared}` is always a permutation of `{0,1,2}`, so no
//! buffer is ever touched by both threads at once. Ownership is transferred
//! only through the atomic swap on `shared`, which carries the happens-before.
//! Result: an atomic, all-or-nothing frame handoff with no mutex, no per-frame
//! allocation, and no partial cross-signal updates.
//!
//! ## Memory ordering
//!
//! - **publish**: write `buffers[write_idx]` (exclusively owned), then
//!   `shared.swap(write_idx | FRESH, AcqRel)`. The *Release* half makes the
//!   buffer writes visible to whoever later acquires `shared`.
//! - **snapshot**: if `shared` has the FRESH bit, `shared.swap(read_idx, AcqRel)`
//!   to claim the freshest frame and hand back our old one. The *Acquire* half
//!   synchronizes-with the producer's release, so the subsequent plain reads of
//!   `buffers[read_idx]` observe the complete, consistent frame.
//!
//! Single producer and single consumer are enforced by construction: `new`
//! returns exactly one [`SignalPublisher`] and one [`SignalReader`].

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use super::schema::{SignalId, SignalSchema};
use super::value::SignalValue;

const INDEX_MASK: usize = 0b011;
const FRESH_BIT: usize = 0b100;

/// The shared triple buffer. Lives behind an `Arc`, cloned into both endpoints.
pub struct SignalStore {
    buffers: [UnsafeCell<Box<[SignalValue]>>; 3],
    /// `(index & INDEX_MASK) | FRESH_BIT` — the slot the consumer should claim.
    shared: AtomicUsize,
    /// Monotonic publish counter (diagnostics / signal-rate metric).
    published: AtomicU64,
    slots: usize,
}

// SAFETY: access to each `buffers[i]` is disjoint by the index-permutation
// invariant above, and ownership transfer goes through `shared`'s AcqRel swaps,
// which establish the necessary happens-before. There is exactly one producer
// and one consumer.
unsafe impl Sync for SignalStore {}
unsafe impl Send for SignalStore {}

impl SignalStore {
    /// Create the store and its single producer/consumer pair. Buffers are
    /// seeded with the schema's zero values.
    pub fn new(schema: &SignalSchema) -> (SignalPublisher, SignalReader) {
        let store = Arc::new(SignalStore {
            buffers: [
                UnsafeCell::new(schema.default_frame()),
                UnsafeCell::new(schema.default_frame()),
                UnsafeCell::new(schema.default_frame()),
            ],
            shared: AtomicUsize::new(2), // write=0, read=1, shared=2
            published: AtomicU64::new(0),
            slots: schema.len(),
        });
        let publisher = SignalPublisher {
            store: store.clone(),
            write_idx: 0,
            working: schema.default_frame(),
            schema: *schema,
        };
        let reader = SignalReader { store, read_idx: 1 };
        (publisher, reader)
    }
}

/// The producer endpoint. Single-writer: lives on the behavior thread.
pub struct SignalPublisher {
    store: Arc<SignalStore>,
    write_idx: usize,
    /// The producer's private, persistent full-frame state. `set` mutates it;
    /// `publish` copies it into the shared buffer. Lets a producer update a
    /// subset of signals without dropping the rest.
    working: Box<[SignalValue]>,
    schema: SignalSchema,
}

impl SignalPublisher {
    /// Stage one signal. Cheap; no publish until [`publish`](Self::publish).
    pub fn set(&mut self, id: SignalId, value: SignalValue) {
        debug_assert_eq!(
            value.kind(),
            self.schema.kind(id),
            "signal `{}` published as {:?}, schema expects {:?}",
            self.schema.name(id),
            value.kind(),
            self.schema.kind(id),
        );
        self.working[id.index()] = value;
    }

    /// Commit the staged frame: copy it into the back buffer and atomically
    /// hand it to the consumer. All staged signals become visible together.
    pub fn publish(&mut self) {
        // SAFETY: `write_idx` is exclusively ours (index-permutation invariant).
        let back = unsafe { &mut *self.store.buffers[self.write_idx].get() };
        back.copy_from_slice(&self.working);

        self.store.published.fetch_add(1, Ordering::Relaxed);
        let old = self
            .store
            .shared
            .swap(self.write_idx | FRESH_BIT, Ordering::AcqRel);
        self.write_idx = old & INDEX_MASK;
    }
}

/// The consumer endpoint. Single-reader: lives on the render thread.
pub struct SignalReader {
    store: Arc<SignalStore>,
    read_idx: usize,
}

impl SignalReader {
    /// Allocate the reusable snapshot buffer (once, at setup).
    pub fn snapshot(&self) -> SignalSnapshot {
        SignalSnapshot {
            values: vec![SignalValue::F32(0.0); self.store.slots].into_boxed_slice(),
        }
    }

    /// Refresh `out` with the latest published frame. Claims the freshest buffer
    /// (if any) then copies it into `out`. No allocation; `out` is reused.
    pub fn snapshot_into(&mut self, out: &mut SignalSnapshot) {
        if self.store.shared.load(Ordering::Acquire) & FRESH_BIT != 0 {
            // Hand back our buffer (no FRESH bit) and claim the published one.
            let old = self.store.shared.swap(self.read_idx, Ordering::AcqRel);
            self.read_idx = old & INDEX_MASK;
        }
        // SAFETY: `read_idx` is exclusively ours (index-permutation invariant).
        let front = unsafe { &*self.store.buffers[self.read_idx].get() };
        out.values.copy_from_slice(front);
    }

    /// Total publishes seen by the store — used to report signal rate.
    pub fn published(&self) -> u64 {
        self.store.published.load(Ordering::Relaxed)
    }
}

/// An immutable, consistent view of all signals for one frame. Owned by the
/// consumer and reused every frame — reading is a flat array index.
pub struct SignalSnapshot {
    values: Box<[SignalValue]>,
}

impl SignalSnapshot {
    #[inline]
    pub fn get(&self, id: SignalId) -> SignalValue {
        self.values[id.index()]
    }

    #[inline]
    #[allow(dead_code)] // part of the snapshot API; consumed once filters bind multiple signals
    pub fn contains(&self, id: SignalId) -> bool {
        id.index() < self.values.len()
    }

    #[allow(dead_code)] // used by the no-allocation test
    pub fn as_ptr(&self) -> *const SignalValue {
        self.values.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn publish_then_snapshot_roundtrips() {
        let schema = SignalSchema::standard();
        let (mut pubr, mut rdr) = SignalStore::new(&schema);
        let id = schema.id("signal.time").unwrap();
        let mut snap = rdr.snapshot();

        // Before any publish: the seeded default.
        rdr.snapshot_into(&mut snap);
        assert_eq!(snap.get(id), SignalValue::F32(0.0));

        pubr.set(id, SignalValue::F32(0.5));
        pubr.publish();
        rdr.snapshot_into(&mut snap);
        assert_eq!(snap.get(id), SignalValue::F32(0.5));
    }

    #[test]
    fn signal_ids_are_stable_and_distinct() {
        let s = SignalSchema::standard();
        let a = s.id("face.position").unwrap();
        let b = s.id("face.position").unwrap();
        assert_eq!(a, b, "id resolution must be stable");
        assert_ne!(
            s.id("face.position"),
            s.id("face.rotation"),
            "distinct names → distinct ids"
        );
        assert_eq!(s.name(a), "face.position");
        assert!(s.id("does.not.exist").is_none());
    }

    /// The core production guarantee: a snapshot can never mix `face.position`
    /// from one publish with `face.rotation` from another. Stress the producer
    /// and consumer on separate threads and assert every snapshot is internally
    /// consistent (this also exercises the publish/snapshot race).
    #[test]
    fn multi_signal_snapshot_is_never_torn() {
        let schema = SignalSchema::standard();
        let pos = schema.id("face.position").unwrap();
        let rot = schema.id("face.rotation").unwrap();
        let (mut pubr, mut rdr) = SignalStore::new(&schema);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_p = stop.clone();
        let producer = thread::spawn(move || {
            let mut k = 0u32;
            while !stop_p.load(Ordering::Relaxed) {
                let f = (k % 1000) as f32; // every signal stamped with the SAME k
                pubr.set(pos, SignalValue::Vec3([f, f, f]));
                pubr.set(rot, SignalValue::Vec4([f, f, f, f]));
                pubr.publish();
                k = k.wrapping_add(1);
            }
        });

        let mut snap = rdr.snapshot();
        for _ in 0..3_000_000 {
            rdr.snapshot_into(&mut snap);
            let p = snap.get(pos).as_vec3().unwrap();
            let r = snap.get(rot).as_vec4().unwrap();
            // position and rotation must come from the same publish.
            assert_eq!(
                p[0], r[0],
                "torn snapshot: face.position={p:?} face.rotation={r:?}"
            );
        }

        stop.store(true, Ordering::Relaxed);
        producer.join().unwrap();
    }

    /// The hot path must not allocate: the reused snapshot buffer never moves.
    #[test]
    fn snapshot_buffer_is_never_reallocated() {
        let schema = SignalSchema::standard();
        let (mut pubr, mut rdr) = SignalStore::new(&schema);
        let id = schema.id("signal.time").unwrap();
        let mut snap = rdr.snapshot();
        let ptr = snap.as_ptr();

        for k in 0..50_000 {
            pubr.set(id, SignalValue::F32(k as f32));
            pubr.publish();
            rdr.snapshot_into(&mut snap);
        }
        assert_eq!(snap.as_ptr(), ptr, "snapshot must reuse its buffer");
    }

    /// Micro-benchmark (ignored by default). Run with:
    /// `cargo test --release -- --ignored --nocapture bench_signal_path`
    #[test]
    #[ignore]
    fn bench_signal_path() {
        let schema = SignalSchema::standard();
        let (mut pubr, mut rdr) = SignalStore::new(&schema);
        let id = schema.id("signal.time").unwrap();
        let mut snap = rdr.snapshot();
        const N: u32 = 5_000_000;

        let t0 = Instant::now();
        for k in 0..N {
            pubr.set(id, SignalValue::F32(k as f32));
            pubr.publish();
        }
        let publish_ns = t0.elapsed().as_nanos() as f64 / N as f64;

        let t1 = Instant::now();
        for _ in 0..N {
            rdr.snapshot_into(&mut snap);
            std::hint::black_box(snap.get(id));
        }
        let snapshot_ns = t1.elapsed().as_nanos() as f64 / N as f64;

        eprintln!(
            "[bench] slots={} publish={publish_ns:.1}ns/op snapshot={snapshot_ns:.1}ns/op \
             store={}B snapshot={}B",
            schema.len(),
            schema.len() * std::mem::size_of::<SignalValue>() * 3,
            schema.len() * std::mem::size_of::<SignalValue>(),
        );
    }
}
