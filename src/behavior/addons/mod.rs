//! Executable behavior **addons** — the reference implementations of the Phase 3
//! external-behavior path.
//!
//! Unlike [`builtins`](super::builtins) (dispatched, historically, by a hardcoded
//! `match`), addons here are bound to their id through the
//! [`BehaviorRegistry`](super::host::BehaviorRegistry) via
//! [`register_behavior_with`](crate::runtime::PipelineRuntime::register_behavior_with):
//! the engine creates them by lookup, never by name. Their manifest + config ship
//! as on-disk packages (`examples/<id>/`, installed to `addons/<id>/`); their
//! executable code is compiled in (v1 loads no scripting/native/wasm). Adding one
//! never edits the engine's dispatch.

// (Empty. Addon behaviors moved to external packages).
