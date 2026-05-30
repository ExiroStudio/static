//! The workspace: the right-hand panel that slides in over the preview in
//! CONFIG mode, plus the modal dialogs. It draws the pipeline as a draggable
//! filter stack, a schema-driven properties panel for the selected node, and
//! the installed-addons list with install/uninstall.
//!
//! To stay clear of borrow-checker tangles (the UI reads `Engine` immutably to
//! lay out, then mutates it to apply edits), we **snapshot** everything we need
//! into owned data first, render from the snapshot collecting *intents*, and
//! apply those intents to the engine after the panels close.

use std::collections::{BTreeMap, HashSet};

use egui::{Align, Align2, Color32, Context, Id, Layout, RichText, Sense, Stroke};

use crate::addon::schema::{ParamMap, ParamSpec, ParamValue};
use crate::engine::Engine;

use super::state::UiState;
use super::widgets;

const ACCENT: Color32 = Color32::from_rgb(96, 140, 220);

struct NodeRow {
    instance_id: String,
    label: String,
    enabled: bool,
    installed: bool,
}

struct PropData {
    instance_id: String,
    addon_name: String,
    specs: BTreeMap<String, ParamSpec>,
    values: ParamMap,
}

struct InstalledRow {
    id: String,
    name: String,
    builtin: bool,
    runnable: bool,
    in_use: bool,
}

/// Intents collected while drawing; applied to the engine after the panels
/// close (when no immutable snapshot borrow is live).
#[derive(Default)]
struct Intents {
    select: Option<String>,
    toggle: Option<(String, bool)>,
    remove: Option<String>,
    reorder: Option<(String, usize)>,
    add: Option<String>,
    uninstall: Option<String>,
    param_edits: Vec<(String, ParamValue)>,
    prop_instance: Option<String>,
}

pub fn draw(ctx: &Context, engine: &mut Engine, state: &mut UiState) {
    let t = ctx.animate_bool_with_time(Id::new("workspace_open"), state.open, 0.15);
    let visible = t > 0.001;

    // ---------- snapshot (immutable borrows of engine end here) ----------
    let used: HashSet<String> = engine
        .config()
        .pipeline
        .iter()
        .map(|n| n.addon.clone())
        .collect();

    let nodes: Vec<NodeRow> = engine
        .config()
        .pipeline
        .iter()
        .map(|n| {
            let (label, installed) = match engine.registry().get(&n.addon) {
                Some(e) => (e.manifest.name.clone(), true),
                None => (n.addon.clone(), false),
            };
            NodeRow {
                instance_id: n.instance_id.clone(),
                label,
                enabled: n.enabled,
                installed,
            }
        })
        .collect();

    let prop: Option<PropData> = state.selected.as_ref().and_then(|id| {
        let node = engine.config().get_node(id)?;
        let entry = engine.registry().get(&node.addon)?;
        Some(PropData {
            instance_id: id.clone(),
            addon_name: entry.manifest.name.clone(),
            specs: entry.manifest.params.clone(),
            values: node.config.clone(),
        })
    });

    let installed: Vec<InstalledRow> = {
        let mut v: Vec<InstalledRow> = engine
            .registry()
            .iter()
            .map(|e| InstalledRow {
                id: e.manifest.id.clone(),
                name: e.manifest.name.clone(),
                builtin: e.builtin,
                runnable: engine.is_runnable(&e.manifest.id),
                in_use: used.contains(&e.manifest.id),
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    };
    let error: Option<String> = engine.last_error().map(str::to_owned);

    let mut intents = Intents {
        prop_instance: prop.as_ref().map(|p| p.instance_id.clone()),
        ..Default::default()
    };

    // ---------- the sliding workspace panel ----------
    if visible {
        egui::SidePanel::right("workspace")
            .exact_width(340.0 * t)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.heading("⚙  Workspace");
                ui.label(RichText::new("webcam  →  filters  →  window").small().weak());
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    pipeline_section(ui, &nodes, &mut intents, state);
                    ui.add_space(10.0);
                    ui.separator();
                    properties_section(ui, &prop, &mut intents);
                    ui.add_space(10.0);
                    ui.separator();
                    installed_section(ui, &installed, state, &mut intents);
                });

                if let Some(notice) = state.notice.clone() {
                    notice_banner(ui, &notice, state);
                }
                if let Some(err) = &error {
                    error_banner(ui, err);
                }
            });
    }

    // ---------- modal dialogs ----------
    if state.show_add {
        add_dialog(ctx, &installed, state, &mut intents);
    }
    if state.confirm_uninstall.is_some() {
        confirm_uninstall_dialog(ctx, state, &mut intents);
    }

    // ---------- apply intents (mutable borrow of engine) ----------
    if let Some(id) = intents.select {
        state.selected = Some(id);
    }
    if let Some((id, enabled)) = intents.toggle {
        engine.set_enabled(&id, enabled);
    }
    if let Some((id, to)) = intents.reorder {
        engine.move_node(&id, to);
    }
    if let Some(id) = intents.remove {
        if state.selected.as_deref() == Some(id.as_str()) {
            state.selected = None;
        }
        engine.remove_node(&id);
    }
    if let Some(addon) = intents.add {
        let new_id = engine.add_node(&addon);
        state.selected = Some(new_id);
        state.show_add = false;
    }
    if let Some(addon) = intents.uninstall {
        match engine.uninstall_addon(&addon) {
            Ok(msg) => state.notice = Some((false, msg)),
            Err(msg) => state.notice = Some((true, msg)),
        }
    }
    if let (Some(p), false) = (&intents.prop_instance, intents.param_edits.is_empty()) {
        for (key, value) in intents.param_edits {
            engine.set_param(p, &key, value);
        }
    }
}

/// The draggable filter stack + "Add Addon" button.
fn pipeline_section(
    ui: &mut egui::Ui,
    nodes: &[NodeRow],
    intents: &mut Intents,
    state: &mut UiState,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("PIPELINE").small().strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("➕ Add Addon").clicked() {
                state.show_add = true;
            }
        });
    });
    ui.add_space(2.0);

    if nodes.is_empty() {
        ui.weak("No filters yet — add one.");
    }

    let dragging = egui::DragAndDrop::has_payload_of_type::<usize>(ui.ctx());
    let mut drop_at: Option<usize> = None;

    for (i, n) in nodes.iter().enumerate() {
        let selected = state.selected.as_deref() == Some(n.instance_id.as_str());
        let fill = if selected {
            Color32::from_rgb(40, 46, 58)
        } else {
            Color32::from_rgb(28, 31, 37)
        };

        let row = egui::Frame::none()
            .fill(fill)
            .inner_margin(7.0)
            .rounding(6.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    // Drag handle — only this grip starts a reorder drag.
                    ui.dnd_drag_source(Id::new(("pipe-drag", n.instance_id.as_str())), i, |ui| {
                        ui.label(RichText::new("⠿").weak().monospace())
                            .on_hover_text("Drag to reorder");
                    });

                    // Enable / disable (the OBS "eye").
                    let eye = if n.enabled { "👁" } else { "🚫" };
                    if ui
                        .add(egui::Button::new(eye).frame(false))
                        .on_hover_text("Enable / disable")
                        .clicked()
                    {
                        intents.toggle = Some((n.instance_id.clone(), !n.enabled));
                    }

                    let mut text = RichText::new(&n.label);
                    if !n.enabled {
                        text = text.weak().strikethrough();
                    }
                    if ui
                        .add(egui::Label::new(text).sense(Sense::click()))
                        .clicked()
                    {
                        intents.select = Some(n.instance_id.clone());
                    }
                    if !n.installed {
                        ui.colored_label(Color32::from_rgb(230, 140, 140), "missing");
                    }

                    // Delete (remove from pipeline, not disk) pinned right.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new("🗑").frame(false))
                            .on_hover_text("Remove from pipeline")
                            .clicked()
                        {
                            intents.remove = Some(n.instance_id.clone());
                        }
                    });
                });
            })
            .response;

        // Drop indicator while dragging.
        if dragging {
            if let Some(p) = ui.ctx().pointer_interact_pos() {
                if row.rect.contains(p) {
                    let after = p.y > row.rect.center().y;
                    drop_at = Some(if after { i + 1 } else { i });
                    let y = if after {
                        row.rect.bottom() + 1.5
                    } else {
                        row.rect.top() - 1.5
                    };
                    ui.painter()
                        .hline(row.rect.x_range(), y, Stroke::new(2.0, ACCENT));
                }
            }
        }
        ui.add_space(3.0);
    }

    // Commit a reorder on drop.
    if ui.input(|i| i.pointer.any_released()) {
        if let Some(payload) = egui::DragAndDrop::take_payload::<usize>(ui.ctx()) {
            let from = *payload;
            if let Some(ins) = drop_at {
                let to = if ins > from { ins - 1 } else { ins };
                if to != from && from < nodes.len() {
                    intents.reorder = Some((nodes[from].instance_id.clone(), to));
                }
            }
        }
    }
}

fn properties_section(ui: &mut egui::Ui, prop: &Option<PropData>, intents: &mut Intents) {
    ui.label(RichText::new("PROPERTIES").small().strong());
    ui.add_space(2.0);
    match prop {
        None => {
            ui.weak("Select a filter above to configure it.");
        }
        Some(p) => {
            ui.label(RichText::new(&p.addon_name).strong());
            if p.specs.is_empty() {
                ui.weak("This addon has no settings.");
            } else {
                properties(ui, p, &mut intents.param_edits);
            }
        }
    }
}

fn installed_section(
    ui: &mut egui::Ui,
    installed: &[InstalledRow],
    state: &mut UiState,
    intents: &mut Intents,
) {
    egui::CollapsingHeader::new("Installed addons")
        .default_open(false)
        .show(ui, |ui| {
            if ui
                .button("📦 Install from ZIP…")
                .on_hover_text("Pick a .zip addon package")
                .clicked()
            {
                state.want_install_picker = true;
            }
            ui.add_space(4.0);

            for a in installed {
                ui.horizontal(|ui| {
                    ui.label(&a.name);
                    if a.builtin {
                        ui.weak("· builtin");
                    } else if !a.runnable {
                        ui.weak("· no code");
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        // Builtins can't be uninstalled (they ship in the binary).
                        if !a.builtin && ui.small_button("Uninstall").clicked() {
                            if a.in_use {
                                state.confirm_uninstall = Some(a.id.clone());
                            } else {
                                intents.uninstall = Some(a.id.clone());
                            }
                        }
                    });
                });
                ui.label(RichText::new(&a.id).small().weak().monospace());
            }
        });
}

/// The "Add Addon" modal: a centered list of installed addons to insert.
fn add_dialog(
    ctx: &Context,
    installed: &[InstalledRow],
    state: &mut UiState,
    intents: &mut Intents,
) {
    egui::Window::new("Add Addon")
        .collapsible(false)
        .resizable(false)
        .movable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label("Available addons");
            ui.add_space(4.0);
            if installed.is_empty() {
                ui.weak("No addons installed.");
            }
            for a in installed {
                let mut button = egui::Button::new(&a.name);
                if !a.runnable {
                    button = button.fill(Color32::from_rgb(45, 38, 30));
                }
                let resp = ui.add_sized([260.0, 28.0], button);
                let resp = if a.runnable {
                    resp
                } else {
                    resp.on_hover_text("Installed but has no code to run yet")
                };
                if resp.clicked() {
                    intents.add = Some(a.id.clone());
                }
            }
            ui.add_space(6.0);
            ui.separator();
            if ui.button("Cancel").clicked() {
                state.show_add = false;
            }
        });
}

fn confirm_uninstall_dialog(ctx: &Context, state: &mut UiState, intents: &mut Intents) {
    let id = state.confirm_uninstall.clone().unwrap_or_default();
    egui::Window::new("Uninstall addon")
        .collapsible(false)
        .resizable(false)
        .movable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(format!("“{id}” is currently used in the active pipeline."));
            ui.label("Uninstalling will remove it from the pipeline too.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new("Uninstall anyway").fill(Color32::from_rgb(120, 40, 40)))
                    .clicked()
                {
                    intents.uninstall = Some(id.clone());
                    state.confirm_uninstall = None;
                }
                if ui.button("Cancel").clicked() {
                    state.confirm_uninstall = None;
                }
            });
        });
}

fn notice_banner(ui: &mut egui::Ui, notice: &(bool, String), state: &mut UiState) {
    let (is_error, msg) = notice;
    let (bg, fg) = if *is_error {
        (Color32::from_rgb(60, 22, 22), Color32::from_rgb(255, 185, 185))
    } else {
        (Color32::from_rgb(22, 44, 30), Color32::from_rgb(180, 240, 200))
    };
    ui.add_space(6.0);
    egui::Frame::none()
        .fill(bg)
        .inner_margin(8.0)
        .rounding(6.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(fg, msg);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("✕").clicked() {
                        state.notice = None;
                    }
                });
            });
        });
}

fn error_banner(ui: &mut egui::Ui, err: &str) {
    ui.add_space(6.0);
    egui::Frame::none()
        .fill(Color32::from_rgb(60, 22, 22))
        .inner_margin(8.0)
        .rounding(6.0)
        .show(ui, |ui| {
            ui.colored_label(Color32::from_rgb(255, 185, 185), err);
        });
}

/// Render the properties for one node, grouped by [`UiHints::group`], entirely
/// from its param schema. Collects `(key, new_value)` edits into `out`.
fn properties(ui: &mut egui::Ui, p: &PropData, out: &mut Vec<(String, ParamValue)>) {
    let mut groups: Vec<(Option<String>, Vec<&String>)> = Vec::new();
    for (key, spec) in &p.specs {
        let g = spec.ui().group.clone();
        match groups.iter_mut().find(|(gg, _)| *gg == g) {
            Some((_, keys)) => keys.push(key),
            None => groups.push((g, vec![key])),
        }
    }

    for (group, keys) in &groups {
        let mut render = |ui: &mut egui::Ui| {
            for key in keys {
                let spec = &p.specs[*key];
                let current = p
                    .values
                    .get(*key)
                    .cloned()
                    .unwrap_or_else(|| spec.default_value());
                if let Some(new_value) = widgets::param_row(ui, key, spec, &current) {
                    out.push(((*key).clone(), new_value));
                }
            }
        };
        match group {
            Some(name) if !name.is_empty() => {
                egui::CollapsingHeader::new(name)
                    .default_open(true)
                    .show(ui, render);
            }
            _ => render(ui),
        }
    }
}
