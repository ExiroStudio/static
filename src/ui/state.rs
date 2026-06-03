//! Persistent UI state — what the workspace remembers between frames.
//!
//! Deliberately small: workspace visibility, the selected node, and transient
//! dialog/notice flags. The pipeline itself is *not* stored here — the engine's
//! [`PipelineConfig`](crate::addon::pipeline::PipelineConfig) is the single
//! source of truth; this only holds view state.

#[derive(Default)]
pub struct UiState {
    /// Whether the config workspace is shown (NORMAL vs CONFIG mode).
    pub open: bool,
    /// `instance_id` of the filter node whose properties are shown, if any.
    pub selected: Option<String>,
    /// `instance_id` of the behavior whose properties are shown, if any.
    pub selected_behavior: Option<String>,
    /// Whether the live Signal Inspector section is expanded.
    pub inspector_open: bool,

    /// "Add filter" dialog is open.
    pub show_add: bool,
    /// "Add behavior" dialog is open.
    pub show_add_behavior: bool,
    /// An addon id pending an uninstall confirmation (it is used in the
    /// pipeline). `None` means no confirmation is being asked.
    pub confirm_uninstall: Option<String>,
    /// The app should open the native ZIP picker after this frame (set by the
    /// "Install" button; handled outside the egui pass to avoid reentrancy).
    pub want_install_picker: bool,
    /// Transient banner: `(is_error, message)`.
    pub notice: Option<(bool, String)>,
}

impl UiState {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}
