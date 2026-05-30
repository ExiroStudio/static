//! Schema → egui widget mapping, plus a calm dark theme.
//!
//! Every widget in the properties panel comes from here. There is no
//! addon-specific code: a control appears only because an addon's manifest
//! declares a [`ParamSpec`] for it. The mapping is the whole contract
//! `manifest → schema → automatic UI`.

use egui::{Color32, Context};

use crate::addon::schema::{ParamSpec, ParamValue};

/// A Discord/OBS-ish dark look: slightly translucent panels over the preview.
pub fn install_theme(ctx: &Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = Color32::from_rgba_unmultiplied(20, 22, 26, 235);
    visuals.panel_fill = Color32::from_rgba_unmultiplied(20, 22, 26, 235);
    visuals.window_rounding = 8.0.into();
    visuals.widgets.noninteractive.rounding = 6.0.into();
    visuals.widgets.inactive.rounding = 6.0.into();
    visuals.widgets.active.rounding = 6.0.into();
    visuals.widgets.hovered.rounding = 6.0.into();
    ctx.set_visuals(visuals);
}

/// Render one parameter row from its [`ParamSpec`] and current value. Returns a
/// new [`ParamValue`] iff the user changed it this frame.
///
/// `ParamValue::F32` wraps an `f64` and `I32` wraps an `i64`, while `ParamSpec`
/// bounds are `f32`/`i32` — the casts below keep both sides honest.
pub fn param_row(
    ui: &mut egui::Ui,
    key: &str,
    spec: &ParamSpec,
    current: &ParamValue,
) -> Option<ParamValue> {
    let label = spec.ui().label.clone().unwrap_or_else(|| key.to_string());
    let help = spec.ui().help.clone();
    let mut out = None;

    ui.horizontal(|ui| {
        let resp = ui.label(label);
        if let Some(h) = &help {
            resp.on_hover_text(h);
        }

        match spec {
            ParamSpec::F32 { min, max, .. } => {
                let mut v = as_f64(current);
                let changed = match (min, max) {
                    (Some(lo), Some(hi)) => ui
                        .add(egui::Slider::new(&mut v, (*lo as f64)..=(*hi as f64)))
                        .changed(),
                    _ => ui.add(egui::DragValue::new(&mut v).speed(0.01)).changed(),
                };
                if changed {
                    out = Some(ParamValue::F32(v));
                }
            }
            ParamSpec::I32 { min, max, .. } => {
                let mut v = as_i64(current);
                let changed = match (min, max) {
                    (Some(lo), Some(hi)) => ui
                        .add(egui::Slider::new(&mut v, (*lo as i64)..=(*hi as i64)).integer())
                        .changed(),
                    _ => ui.add(egui::DragValue::new(&mut v).speed(1.0)).changed(),
                };
                if changed {
                    out = Some(ParamValue::I32(v));
                }
            }
            ParamSpec::Bool { .. } => {
                let mut b = matches!(current, ParamValue::Bool(true));
                if ui.checkbox(&mut b, "").changed() {
                    out = Some(ParamValue::Bool(b));
                }
            }
            ParamSpec::Enum { values, default, .. } => {
                let mut sel = match current {
                    ParamValue::Str(s) if values.contains(s) => s.clone(),
                    _ => default.clone(),
                };
                let before = sel.clone();
                egui::ComboBox::from_id_source((key, "enum"))
                    .selected_text(&sel)
                    .show_ui(ui, |ui| {
                        for v in values {
                            ui.selectable_value(&mut sel, v.clone(), v);
                        }
                    });
                if sel != before {
                    out = Some(ParamValue::Str(sel));
                }
            }
            ParamSpec::Color { .. } => {
                let mut color = parse_color(current);
                if ui.color_edit_button_srgba(&mut color).changed() {
                    out = Some(ParamValue::Str(emit_color(color)));
                }
            }
            ParamSpec::Text { .. } => {
                let mut s = match current {
                    ParamValue::Str(s) => s.clone(),
                    _ => String::new(),
                };
                if ui.text_edit_singleline(&mut s).changed() {
                    out = Some(ParamValue::Str(s));
                }
            }
        }
    });

    out
}

fn as_f64(v: &ParamValue) -> f64 {
    match v {
        ParamValue::F32(x) => *x,
        ParamValue::I32(i) => *i as f64,
        _ => 0.0,
    }
}

fn as_i64(v: &ParamValue) -> i64 {
    match v {
        ParamValue::I32(i) => *i,
        ParamValue::F32(x) => *x as i64,
        _ => 0,
    }
}

/// Parse a `#rrggbb` / `#rrggbbaa` string into a colour (defaults to white on
/// anything unexpected — the schema validates the real values).
fn parse_color(v: &ParamValue) -> Color32 {
    let ParamValue::Str(s) = v else {
        return Color32::WHITE;
    };
    let hex = s.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(255);
    match hex.len() {
        6 => Color32::from_rgb(byte(0), byte(2), byte(4)),
        8 => Color32::from_rgba_unmultiplied(byte(0), byte(2), byte(4), byte(6)),
        _ => Color32::WHITE,
    }
}

fn emit_color(c: Color32) -> String {
    if c.a() == 255 {
        format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", c.r(), c.g(), c.b(), c.a())
    }
}
