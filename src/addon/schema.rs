//! Parameter schema for addon configuration.
//!
//! Each addon declares its configurable parameters in its manifest under the
//! `[params.<key>]` tables. The schema serves three jobs at once:
//!
//!   1. Validation — reject config values that violate type/range/enum rules.
//!   2. Defaults   — provide a known value for any unset key.
//!   3. UI hints   — drive a future auto-generated config editor (label, group).
//!
//! Storage of *current* parameter values uses [`ParamValue`], a typed enum
//! (not `Box<dyn Any>`) so values round-trip through JSON cleanly and remain
//! introspectable.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A single parameter declaration. The tag-on-`type` representation matches
/// the TOML layout `type = "f32" / "i32" / "bool" / "enum" / "color" / "text"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamSpec {
    F32 {
        default: f32,
        #[serde(default)]
        min: Option<f32>,
        #[serde(default)]
        max: Option<f32>,
        #[serde(default, flatten)]
        ui: UiHints,
    },
    I32 {
        default: i32,
        #[serde(default)]
        min: Option<i32>,
        #[serde(default)]
        max: Option<i32>,
        #[serde(default, flatten)]
        ui: UiHints,
    },
    Bool {
        default: bool,
        #[serde(default, flatten)]
        ui: UiHints,
    },
    Enum {
        default: String,
        values: Vec<String>,
        #[serde(default, flatten)]
        ui: UiHints,
    },
    Color {
        /// `#rrggbb` or `#rrggbbaa`.
        default: String,
        #[serde(default, flatten)]
        ui: UiHints,
    },
    Text {
        default: String,
        #[serde(default, flatten)]
        ui: UiHints,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiHints {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub help: Option<String>,
}

/// A concrete parameter value, as carried in a pipeline config. `untagged`
/// keeps the JSON shape compact (`"intensity": 0.8`, `"mode": "soft"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Bool(bool),
    I32(i64),
    F32(f64),
    Str(String),
}

pub type ParamMap = BTreeMap<String, ParamValue>;

impl ParamSpec {
    pub fn default_value(&self) -> ParamValue {
        match self {
            Self::F32 { default, .. } => ParamValue::F32(*default as f64),
            Self::I32 { default, .. } => ParamValue::I32(*default as i64),
            Self::Bool { default, .. } => ParamValue::Bool(*default),
            Self::Enum { default, .. } => ParamValue::Str(default.clone()),
            Self::Color { default, .. } => ParamValue::Str(default.clone()),
            Self::Text { default, .. } => ParamValue::Str(default.clone()),
        }
    }

    /// Validate that `v` matches this spec's type and range/enum constraints.
    /// JSON `1` deserializes as `I32` even where an `F32` is expected, so we
    /// accept both numeric variants for numeric specs.
    pub fn validate(&self, v: &ParamValue) -> std::result::Result<(), String> {
        match (self, v) {
            (Self::F32 { min, max, .. }, ParamValue::F32(x)) => {
                check_range(*x, min.map(|m| m as f64), max.map(|m| m as f64))
            }
            (Self::F32 { min, max, .. }, ParamValue::I32(i)) => {
                check_range(*i as f64, min.map(|m| m as f64), max.map(|m| m as f64))
            }
            (Self::I32 { min, max, .. }, ParamValue::I32(i)) => {
                check_range(*i, min.map(|m| m as i64), max.map(|m| m as i64))
            }
            (Self::Bool { .. }, ParamValue::Bool(_)) => Ok(()),
            (Self::Enum { values, .. }, ParamValue::Str(s)) => {
                if values.iter().any(|v| v == s) {
                    Ok(())
                } else {
                    Err(format!("value {s:?} not in enum {values:?}"))
                }
            }
            (Self::Color { .. }, ParamValue::Str(s)) => validate_color(s),
            (Self::Text { .. }, ParamValue::Str(_)) => Ok(()),
            (spec, val) => Err(format!(
                "type mismatch: expected {}, got {}",
                spec.type_name(),
                value_type_name(val)
            )),
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Self::F32 { .. } => "f32",
            Self::I32 { .. } => "i32",
            Self::Bool { .. } => "bool",
            Self::Enum { .. } => "enum",
            Self::Color { .. } => "color",
            Self::Text { .. } => "text",
        }
    }

    pub fn ui(&self) -> &UiHints {
        match self {
            Self::F32 { ui, .. }
            | Self::I32 { ui, .. }
            | Self::Bool { ui, .. }
            | Self::Enum { ui, .. }
            | Self::Color { ui, .. }
            | Self::Text { ui, .. } => ui,
        }
    }
}

fn value_type_name(v: &ParamValue) -> &'static str {
    match v {
        ParamValue::Bool(_) => "bool",
        ParamValue::I32(_) => "int",
        ParamValue::F32(_) => "float",
        ParamValue::Str(_) => "string",
    }
}

fn check_range<T: PartialOrd + std::fmt::Display + Copy>(
    x: T,
    min: Option<T>,
    max: Option<T>,
) -> std::result::Result<(), String> {
    if let Some(m) = min {
        if x < m {
            return Err(format!("value {x} below min {m}"));
        }
    }
    if let Some(m) = max {
        if x > m {
            return Err(format!("value {x} above max {m}"));
        }
    }
    Ok(())
}

fn validate_color(s: &str) -> std::result::Result<(), String> {
    if !s.starts_with('#') || (s.len() != 7 && s.len() != 9) {
        return Err(format!("invalid color {s:?} (expected #rrggbb or #rrggbbaa)"));
    }
    if !s[1..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("invalid color {s:?} (non-hex digits)"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f32_spec(min: f32, max: f32, default: f32) -> ParamSpec {
        ParamSpec::F32 {
            default,
            min: Some(min),
            max: Some(max),
            ui: UiHints::default(),
        }
    }

    #[test]
    fn f32_range_validation() {
        let s = f32_spec(0.0, 1.0, 0.5);
        assert!(s.validate(&ParamValue::F32(0.5)).is_ok());
        assert!(s.validate(&ParamValue::F32(-0.1)).is_err());
        assert!(s.validate(&ParamValue::F32(1.5)).is_err());
        // JSON-integer should still validate against an f32 spec.
        assert!(s.validate(&ParamValue::I32(1)).is_ok());
    }

    #[test]
    fn enum_validation() {
        let s = ParamSpec::Enum {
            default: "soft".into(),
            values: vec!["soft".into(), "hard".into()],
            ui: UiHints::default(),
        };
        assert!(s.validate(&ParamValue::Str("soft".into())).is_ok());
        assert!(s.validate(&ParamValue::Str("nope".into())).is_err());
    }

    #[test]
    fn color_validation() {
        let s = ParamSpec::Color {
            default: "#000000".into(),
            ui: UiHints::default(),
        };
        assert!(s.validate(&ParamValue::Str("#abcdef".into())).is_ok());
        assert!(s.validate(&ParamValue::Str("#abcdef12".into())).is_ok());
        assert!(s.validate(&ParamValue::Str("abcdef".into())).is_err());
        assert!(s.validate(&ParamValue::Str("#zzzzzz".into())).is_err());
    }

    #[test]
    fn type_mismatch_is_rejected() {
        let s = f32_spec(0.0, 1.0, 0.5);
        assert!(s.validate(&ParamValue::Bool(true)).is_err());
        assert!(s.validate(&ParamValue::Str("x".into())).is_err());
    }
}
