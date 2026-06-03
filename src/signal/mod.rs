//! Signal runtime — the producer→consumer channel between behaviors and filters.
//!
//! Behaviors (on the behavior thread) stage signals on a [`SignalPublisher`] and
//! `publish()` a whole frame; the filter runtime (on the render thread) takes
//! one [`SignalSnapshot`] per frame through a [`SignalReader`] and every filter
//! reads from it by [`SignalId`]. The two sides share only the lock-free
//! [`SignalStore`] — no mutex, no channels, no `Box<dyn Any>`, and no allocation
//! on the per-frame path.
//!
//! - [`value`]  — `SignalValue` / `SignalKind`: the fixed, `Copy` payload.
//! - [`schema`] — `SignalSchema` / `SignalId`: name ↔ slot ↔ type, resolved once.
//! - [`store`]  — `SignalStore` / `SignalPublisher` / `SignalReader` /
//!   `SignalSnapshot`: the triple-buffered, consistent frame handoff.

mod schema;
mod store;
mod value;

pub use schema::{
    SchemaError, SignalId, SignalRef, SignalSchema, SignalSchemaBuilder, SignalSpec,
};
pub use store::{SignalPublisher, SignalReader, SignalSnapshot, SignalStore};
pub use value::{SignalKind, SignalValue};
