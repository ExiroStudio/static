//! Manual runtime rebuild on config change.
//!
//! "Auto-reload" here is deliberately simple: a UI edit mutates the engine's
//! in-memory [`PipelineConfig`] and marks state dirty; once the dirt has
//! *settled* (a short debounce), [`Engine::tick_reload`] calls
//! [`PipelineRuntime::build`](crate::runtime::PipelineRuntime::build) again.
//! `build` validates and atomically swaps the live node list, so a rejected
//! rebuild leaves the previous pipeline running untouched — there is no
//! hot-reload machinery, the rebuild *is* the reload.
//!
//! Debounce matters for slider drags: without it every dragged frame would
//! reinstantiate every node. Discrete edits (toggles) ask to apply on the next
//! frame instead of waiting out the debounce.

use std::time::{Duration, Instant};

use crate::addon::AddonError;

const DEBOUNCE: Duration = Duration::from_millis(120);

pub struct ReloadState {
    dirty_since: Option<Instant>,
    last_error: Option<String>,
}

impl ReloadState {
    pub fn new() -> Self {
        Self {
            dirty_since: None,
            last_error: None,
        }
    }

    /// Continuous edit (e.g. a slider drag): restart the debounce timer so the
    /// rebuild fires ~`DEBOUNCE` after the user stops moving.
    pub fn mark_dirty(&mut self) {
        self.dirty_since = Some(Instant::now());
    }

    /// Discrete edit (e.g. an enable toggle): apply on the next frame rather
    /// than waiting out the debounce window.
    pub fn mark_dirty_now(&mut self) {
        self.dirty_since = Some(Instant::now() - DEBOUNCE);
    }

    /// Returns `true` exactly once when a pending edit has settled, consuming
    /// the dirty flag.
    pub fn take_if_settled(&mut self) -> bool {
        match self.dirty_since {
            Some(t) if t.elapsed() >= DEBOUNCE => {
                self.dirty_since = None;
                true
            }
            _ => false,
        }
    }

    pub fn set_error(&mut self, msg: Option<String>) {
        self.last_error = msg;
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

impl Default for ReloadState {
    fn default() -> Self {
        Self::new()
    }
}

/// Turn an [`AddonError`] into a plain-language message a non-technical user can
/// act on. No engine vocabulary leaks through.
pub fn humanize(e: &AddonError) -> String {
    use AddonError::*;
    match e {
        PipelineRejected(s) => format!("Some settings are invalid:\n{s}"),
        UnsupportedSource(k) => format!("Unknown input source '{k}'."),
        UnsupportedSink(k) => format!("Unknown output '{k}'."),
        NoImplementation(id) => format!("Addon '{id}' is installed but has no code to run."),
        InvalidPipeline(s) => format!("This setup can't be applied: {s}"),
        other => format!("Couldn't apply changes: {other}"),
    }
}
