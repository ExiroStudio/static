//! The workspace: the right-hand panel that slides in over the preview in
//! CONFIG mode. It draws the pipeline as a filter stack, a schema-driven
//! properties panel for the selected node, and a read-only list of installed
//! addons.
//!
//! To stay clear of borrow-checker tangles (the UI reads `Engine` immutably to
//! lay out, then mutates it to apply edits), we **snapshot** everything we need
//! into owned data first, render from the snapshot collecting *intents*, and
//! apply those intents to the engine after the panel closes.

use std::collections::BTreeMap;

use egui::{Color32, Context, Id, RichText};

use crate::addon::schema::{ParamMap, ParamSpec, ParamValue};
use crate::engine::Engine;

use super::state::UiState;
use super::widgets;

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
}

pub fn draw(ctx: &Context, engine: &mut Engine, state: &mut UiState) {
    // Animate the slide; when fully closed there is nothing to draw.
    let t = ctx.animate_bool_with_time(Id::new("workspace_open"), state.open, 0.15);
    if t <= 0.001 {
        return;
    }

    // ---------- snapshot (immutable borrows of engine end here) ----------
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
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    };
    let rejected: Vec<String> = engine
        .registry()
        .rejected()
        .iter()
        .map(|r| r.reason.clone())
        .collect();
    let error: Option<String> = engine.last_error().map(str::to_owned);

    // ---------- intents collected during draw ----------
    let mut toggle: Option<(String, bool)> = None;
    let mut select: Option<String> = None;
    let mut param_edits: Vec<(String, ParamValue)> = Vec::new();

    egui::SidePanel::right("workspace")
        .exact_width(340.0 * t)
        .resizable(false)
        .show(ctx, |ui| {
            ui.add_space(6.0);
            ui.heading("⚙  Workspace");
            ui.label(
                RichText::new("webcam  →  filters  →  window")
                    .small()
                    .weak(),
            );
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                // ----- Pipeline (filter stack) -----
                ui.label(RichText::new("PIPELINE").small().strong());
                ui.add_space(2.0);
                if nodes.is_empty() {
                    ui.weak("No filters in the pipeline.");
                }
                for n in &nodes {
                    let selected = state.selected.as_deref() == Some(n.instance_id.as_str());
                    let card = node_card(ui, n, selected);
                    if let Some(en) = card.0 {
                        toggle = Some((n.instance_id.clone(), en));
                    }
                    if card.1 {
                        select = Some(n.instance_id.clone());
                    }
                }

                ui.add_space(10.0);
                ui.separator();

                // ----- Properties (generated from the addon's schema) -----
                ui.label(RichText::new("PROPERTIES").small().strong());
                ui.add_space(2.0);
                match &prop {
                    None => {
                        ui.weak("Select a filter above to configure it.");
                    }
                    Some(p) => {
                        ui.label(RichText::new(&p.addon_name).strong());
                        if p.specs.is_empty() {
                            ui.weak("This addon has no settings.");
                        } else {
                            properties(ui, p, &mut param_edits);
                        }
                    }
                }

                ui.add_space(10.0);
                ui.separator();

                // ----- Installed addons (read-only) -----
                egui::CollapsingHeader::new("Installed addons")
                    .default_open(false)
                    .show(ui, |ui| {
                        for a in &installed {
                            ui.horizontal(|ui| {
                                ui.monospace(&a.id);
                                if a.builtin {
                                    ui.weak("builtin");
                                }
                            });
                        }
                        if !rejected.is_empty() {
                            ui.add_space(4.0);
                            ui.colored_label(Color32::from_rgb(230, 140, 140), "Rejected:");
                            for r in &rejected {
                                ui.weak(r);
                            }
                        }
                    });
            });

            // ----- error banner (plain language; previous look stays live) -----
            if let Some(err) = &error {
                ui.add_space(6.0);
                egui::Frame::none()
                    .fill(Color32::from_rgb(60, 22, 22))
                    .inner_margin(8.0)
                    .rounding(6.0)
                    .show(ui, |ui| {
                        ui.colored_label(Color32::from_rgb(255, 185, 185), err);
                    });
            }
        });

    // ---------- apply intents (mutable borrow of engine) ----------
    if let Some(id) = select {
        state.selected = Some(id);
    }
    if let Some((id, en)) = toggle {
        engine.set_enabled(&id, en);
    }
    if let Some(p) = &prop {
        for (key, value) in param_edits {
            engine.set_param(&p.instance_id, &key, value);
        }
    }
}

/// One filter card. Returns `(toggled_to, selected_this_frame)`.
fn node_card(ui: &mut egui::Ui, n: &NodeRow, selected: bool) -> (Option<bool>, bool) {
    let mut toggled = None;
    let mut clicked = false;

    let fill = if selected {
        Color32::from_rgb(40, 46, 58)
    } else {
        Color32::from_rgb(28, 31, 37)
    };

    ui.add_space(3.0);
    egui::Frame::none()
        .fill(fill)
        .inner_margin(7.0)
        .rounding(6.0)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                // Enable / disable (the OBS "eye").
                let eye = if n.enabled { "👁" } else { "🚫" };
                if ui
                    .add(egui::Button::new(eye).frame(false))
                    .on_hover_text("Enable / disable")
                    .clicked()
                {
                    toggled = Some(!n.enabled);
                }

                let mut text = RichText::new(&n.label);
                if !n.enabled {
                    text = text.weak().strikethrough();
                }
                if ui
                    .add(egui::Label::new(text).sense(egui::Sense::click()))
                    .clicked()
                {
                    clicked = true;
                }

                if !n.installed {
                    ui.colored_label(Color32::from_rgb(230, 140, 140), "missing");
                }
            });
        });

    (toggled, clicked)
}

/// Render the properties for one node, grouped by [`UiHints::group`], entirely
/// from its param schema. Collects `(key, new_value)` edits into `out`.
fn properties(ui: &mut egui::Ui, p: &PropData, out: &mut Vec<(String, ParamValue)>) {
    // Group keys by UiHints.group, preserving manifest (BTreeMap) order.
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
