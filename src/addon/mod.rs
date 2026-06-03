//! Addon ecosystem (v1) — manifest, registry, pipeline configuration.
//!
//! Layer between the engine runtime and the installed addons on disk.
//! Concerned only with *describing* and *organising* addons; addon execution
//! is the runtime's job (later phase) and lives elsewhere.
//!
//! Module map:
//!
//!   * [`manifest`] — TOML manifest parsing and structural validation.
//!   * [`schema`]   — addon parameter declarations (`ParamSpec`, `ParamValue`).
//!   * [`registry`] — filesystem scan of an addons root directory.
//!   * [`pipeline`] — `pipeline.json` document + editing operations.
//!   * [`compat`]   — engine ↔ addon API version checks.
//!   * [`package`]  — `.starpkg` archive format (extraction stubbed in v1).
//!   * [`error`]    — unified error type returned across the module.

pub mod compat;
pub mod error;
pub mod manifest;
pub mod package;
pub mod pipeline;
pub mod registry;
pub mod schema;

// The unified error/result is the one type used across module boundaries by
// name; everything else is reached through its own submodule path
// (`addon::manifest::Manifest`, `addon::pipeline::PipelineConfig`, …) to keep
// the import sites self-documenting and the facade minimal.
pub use error::{AddonError, Result};
