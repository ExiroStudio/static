//! Addon manifest (v1).
//!
//! TOML document at the root of every installed addon. Engine-side source of
//! truth for: identity, version, API compatibility, declared shaders/assets,
//! configurable parameters, and requested permissions. The runtime never
//! discovers anything by scanning the addon directory — if it's not in the
//! manifest, it doesn't exist.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::{AddonError, Result};
use super::schema::ParamSpec;
use crate::signal::{SignalRef, SignalSpec};

pub const MANIFEST_FILENAME: &str = "manifest.toml";

/// Format-version of the manifest *itself* (not the addon). Bumped only on
/// breaking changes to this TOML schema.
pub const CURRENT_MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest_version: u32,

    // ---- identity ----
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,

    // ---- engine compatibility ----
    pub api_min: u32,
    pub api_max: u32,

    // ---- kind ----
    pub kind: AddonKind,

    // ---- declarations ----
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub shaders: Vec<ShaderDecl>,
    #[serde(default)]
    pub assets: Vec<AssetDecl>,
    #[serde(default)]
    pub params: BTreeMap<String, ParamSpec>,

    // ---- signals ----
    /// Signals this addon publishes (behaviors).
    #[serde(default)]
    pub publish: Vec<SignalSpec>,
    /// Signals this addon consumes (filters).
    #[serde(default)]
    pub consume: Vec<SignalRef>,
}

/// What kind of addon this is. Pipeline (filter) addons run on the render
/// thread; behavior addons are producers that run on the behavior thread and
/// only publish signals. Sources and sinks remain engine-shipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddonKind {
    Pipeline,
    Behavior,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub filesystem: FilesystemPerm,
    #[serde(default)]
    pub network: NetworkPerm,
    #[serde(default)]
    pub gpu: GpuPerm,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemPerm {
    #[default]
    None,
    /// Read-only access to files bundled inside the addon's own directory.
    AddonLocal,
    /// Read access to host paths chosen by the user at install time.
    Host,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPerm {
    #[default]
    None,
    Http,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuPerm {
    Default,
    /// Compute shaders, larger texture allocations, etc.
    Extended,
}

impl Default for GpuPerm {
    fn default() -> Self {
        Self::Default
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShaderDecl {
    pub id: String,
    pub path: String,
    #[serde(default = "default_shader_stage")]
    pub stage: String,
    #[serde(default = "default_shader_entry")]
    pub entry: String,
}

fn default_shader_stage() -> String {
    "fragment".into()
}
fn default_shader_entry() -> String {
    "fs_main".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetDecl {
    pub id: String,
    pub path: String,
    #[serde(default = "default_asset_kind")]
    pub kind: String,
}

fn default_asset_kind() -> String {
    "binary".into()
}

impl Manifest {
    /// Load and validate a manifest at an exact file path.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read_to_string(path).map_err(|e| AddonError::ManifestIo {
            path: path.into(),
            source: e,
        })?;
        let manifest: Manifest = toml::from_str(&bytes).map_err(|e| AddonError::ManifestParse {
            path: path.into(),
            source: e,
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Load `manifest.toml` from an addon directory.
    pub fn from_dir(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST_FILENAME);
        if !path.exists() {
            return Err(AddonError::ManifestNotFound(path));
        }
        Self::load(&path)
    }

    /// Structural validation: required fields, well-formed id, version
    /// monotonicity, and self-consistency of declared params (their defaults
    /// must satisfy their own spec).
    pub fn validate(&self) -> Result<()> {
        if self.manifest_version != CURRENT_MANIFEST_VERSION {
            return Err(AddonError::ManifestInvalid(format!(
                "unsupported manifest_version {} (this engine expects {})",
                self.manifest_version, CURRENT_MANIFEST_VERSION
            )));
        }
        if self.id.is_empty() {
            return Err(AddonError::ManifestInvalid("addon id is empty".into()));
        }
        if !is_valid_id(&self.id) {
            return Err(AddonError::ManifestInvalid(format!(
                "addon id {:?} contains invalid characters (allowed: a-z 0-9 . _ -, lowercase only)",
                self.id
            )));
        }
        if self.name.trim().is_empty() {
            return Err(AddonError::ManifestInvalid("addon name is empty".into()));
        }
        if self.version.trim().is_empty() {
            return Err(AddonError::ManifestInvalid("addon version is empty".into()));
        }
        if self.api_min > self.api_max {
            return Err(AddonError::ManifestInvalid(format!(
                "api_min ({}) > api_max ({})",
                self.api_min, self.api_max
            )));
        }

        // Declarations: unique ids
        check_unique_ids("shader", self.shaders.iter().map(|s| s.id.as_str()))?;
        check_unique_ids("asset", self.assets.iter().map(|a| a.id.as_str()))?;

        // Param defaults must satisfy their own schema.
        for (key, spec) in &self.params {
            let default = spec.default_value();
            spec.validate(&default).map_err(|e| {
                AddonError::ManifestInvalid(format!(
                    "param {key:?}: default value invalid — {e}"
                ))
            })?;
            // If it's an enum, default must be in the values list.
            if let ParamSpec::Enum { default, values, .. } = spec {
                if !values.iter().any(|v| v == default) {
                    return Err(AddonError::ManifestInvalid(format!(
                        "param {key:?}: enum default {default:?} not in values {values:?}"
                    )));
                }
            }
        }

        Ok(())
    }

}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-'
        })
}

fn check_unique_ids<'a>(kind: &str, ids: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(AddonError::ManifestInvalid(format!(
                "duplicate {kind} id {id:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
manifest_version = 1

id          = "io.static.crt"
name        = "CRT"
version     = "1.0.0"
author      = "Alice"
description = "Classic CRT distortion"

api_min = 1
api_max = 1
kind    = "pipeline"

[permissions]
filesystem = "none"
network    = "none"
gpu        = "default"

[[shaders]]
id   = "main"
path = "shaders/crt.wgsl"

[[assets]]
id   = "preset_soft"
path = "assets/presets/soft.json"
kind = "preset"

[params.intensity]
type    = "f32"
default = 1.0
min     = 0.0
max     = 2.0
label   = "Intensity"
group   = "Look"

[params.mode]
type    = "enum"
default = "soft"
values  = ["soft", "hard", "off"]
"#;

    #[test]
    fn parses_sample_manifest() {
        let m: Manifest = toml::from_str(SAMPLE).expect("parse");
        m.validate().expect("validate");
        assert_eq!(m.id, "io.static.crt");
        assert_eq!(m.api_min, 1);
        assert_eq!(m.api_max, 1);
        assert_eq!(m.kind, AddonKind::Pipeline);
        assert_eq!(m.shaders.len(), 1);
        assert_eq!(m.shaders[0].stage, "fragment"); // default
        assert_eq!(m.shaders[0].entry, "fs_main"); // default
        assert_eq!(m.params.len(), 2);
    }

    #[test]
    fn rejects_bad_id() {
        let mut m: Manifest = toml::from_str(SAMPLE).unwrap();
        m.id = "Has Space".into();
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_inverted_api_range() {
        let mut m: Manifest = toml::from_str(SAMPLE).unwrap();
        m.api_min = 5;
        m.api_max = 2;
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_enum_default_outside_values() {
        let toml_src = r#"
manifest_version = 1
id = "x"
name = "X"
version = "1.0"
author = "A"
api_min = 1
api_max = 1
kind = "pipeline"

[params.mode]
type = "enum"
default = "elsewhere"
values = ["soft", "hard"]
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_shader_ids() {
        let toml_src = r#"
manifest_version = 1
id = "x"
name = "X"
version = "1.0"
author = "A"
api_min = 1
api_max = 1
kind = "pipeline"

[[shaders]]
id   = "main"
path = "a.wgsl"

[[shaders]]
id   = "main"
path = "b.wgsl"
"#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert!(m.validate().is_err());
    }
}
