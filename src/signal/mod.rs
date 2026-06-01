//! SignalBus — the producer→consumer channel between behaviors and filters.
//!
//! Behaviors (on the behavior thread) `publish` named values; the filter
//! runtime (on the render thread) takes one immutable `snapshot` per frame and
//! every filter reads from it. The two never reference each other — they share
//! only this bus.
//!
//! Spike scope: a single value type (`F32`) and a fixed, known set of signal
//! names ([`SIGNALS`]). There is no schema file and no dynamic registration —
//! adding a signal means adding a name here. The store is a flat array of
//! atomics: `publish` is a lock-free store, `snapshot` a lock-free copy onto the
//! stack. No heap allocation per frame, no `Box<dyn Any>`, no channels.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// A signal value. The spike carries scalars only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SignalValue {
    F32(f32),
}

/// The fixed set of signals the bus carries. The array length of the store is
/// derived from this, so an index is stable for the program's life.
pub const SIGNALS: &[&str] = &["signal.time"];

#[inline]
fn index(name: &str) -> Option<usize> {
    SIGNALS.iter().position(|&s| s == name)
}

/// Shared producer→consumer store. One `AtomicU32` slot (f32 bits) per known
/// signal, plus a published counter for diagnostics.
pub struct SignalBus {
    slots: [AtomicU32; SIGNALS.len()],
    published: AtomicU64,
}

impl SignalBus {
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| AtomicU32::new(0)),
            published: AtomicU64::new(0),
        }
    }

    /// Publish a value. Unknown names are ignored (no panic on the hot path).
    /// Lock-free: a single relaxed store. Safe to call from any thread.
    pub fn publish(&self, name: &str, value: SignalValue) {
        if let Some(i) = index(name) {
            let SignalValue::F32(v) = value;
            self.slots[i].store(v.to_bits(), Ordering::Relaxed);
            self.published.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Take an immutable, stack-allocated copy of all signals for this frame.
    /// All filters in a frame read from one snapshot → internally consistent.
    pub fn snapshot(&self) -> SignalSnapshot {
        let mut values = [0.0f32; SIGNALS.len()];
        for (i, slot) in self.slots.iter().enumerate() {
            values[i] = f32::from_bits(slot.load(Ordering::Relaxed));
        }
        SignalSnapshot { values }
    }

    /// Total publishes since start — used to report signal frequency.
    pub fn published_count(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }
}

impl Default for SignalBus {
    fn default() -> Self {
        Self::new()
    }
}

/// An immutable, `Copy` snapshot of the bus for one frame. Reading is a flat
/// array lookup — no locks, no allocation.
#[derive(Clone, Copy)]
pub struct SignalSnapshot {
    values: [f32; SIGNALS.len()],
}

impl SignalSnapshot {
    /// The latest published value for `name`, or `None` if it is not a known
    /// signal.
    pub fn get(&self, name: &str) -> Option<f32> {
        index(name).map(|i| self.values[i])
    }
}
