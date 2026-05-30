//! Persistent UI state — what the workspace remembers between frames.
//!
//! Deliberately tiny: whether the workspace is open and which node is selected
//! for configuration. The pipeline itself is *not* stored here — the engine's
//! [`PipelineConfig`](crate::addon::pipeline::PipelineConfig) is the single
//! source of truth; this only holds view state.

#[derive(Default)]
pub struct UiState {
    /// Whether the config workspace is shown (NORMAL vs CONFIG mode).
    pub open: bool,
    /// `instance_id` of the node whose properties are shown, if any.
    pub selected: Option<String>,
}

impl UiState {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}
