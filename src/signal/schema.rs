//! [`SignalSchema`] — the name ↔ slot ↔ type table.
//!
//! Names are resolved to a [`SignalId`] (a slot index) **once**, at behavior
//! start or filter instantiation. The hot path then addresses signals by id —
//! a flat array index, never a string compare.
//!
//! For this foundation the schema is a fixed, code-declared table. Phase 2 will
//! build it from behavior/filter manifests instead; the *shape* of the API
//! (`id` / `name` / `kind` / `len` / `default_frame`) is what the rest of the
//! runtime depends on and will not change.

use super::value::{SignalKind, SignalValue};

/// A resolved signal slot. Stable for the life of a schema; cheap to copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignalId(u16);

impl SignalId {
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// The standard signal set. Adding a signal is one row here. Order defines the
/// slot index, so ids are stable as long as this table is append-only.
const STANDARD: &[(&str, SignalKind)] = &[
    ("signal.time", SignalKind::F32),
    ("face.position", SignalKind::Vec3),
    ("face.rotation", SignalKind::Vec4),
    ("face.scale", SignalKind::F32),
    ("audio.level", SignalKind::F32),
    ("audio.bass", SignalKind::F32),
    ("entity.expression", SignalKind::I32),
];

/// A name/slot/type table. Holds a `&'static` row slice, so it is `Copy` and
/// free to pass by value when resolving ids.
#[derive(Clone, Copy)]
pub struct SignalSchema {
    rows: &'static [(&'static str, SignalKind)],
}

impl SignalSchema {
    /// The process-wide standard schema. (Phase 2 replaces this with a schema
    /// built from manifests.)
    pub fn standard() -> SignalSchema {
        SignalSchema { rows: STANDARD }
    }

    /// Number of slots — the length of every signal frame and snapshot.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Resolve a name to its slot. Call once, off the hot path.
    pub fn id(&self, name: &str) -> Option<SignalId> {
        self.rows
            .iter()
            .position(|(n, _)| *n == name)
            .map(|i| SignalId(i as u16))
    }

    pub fn name(&self, id: SignalId) -> &'static str {
        self.rows[id.index()].0
    }

    pub fn kind(&self, id: SignalId) -> SignalKind {
        self.rows[id.index()].1
    }

    /// A fresh frame seeded with each slot's zero value, sized to the schema.
    /// Allocated once per producer/consumer at setup — never on the hot path.
    pub fn default_frame(&self) -> Box<[SignalValue]> {
        self.rows
            .iter()
            .map(|(_, k)| SignalValue::default_for(*k))
            .collect()
    }
}
