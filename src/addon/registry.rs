//! Addon registry: filesystem-backed index of installed addons.
//!
//! The registry scans a single root directory whose direct children are
//! addon directories (one per installed addon, named by `addon.id`). Each
//! child must contain a `manifest.toml` at its root. The registry loads
//! manifests, validates them, runs API compatibility checks, and indexes
//! the valid ones by id. Failed addons are collected separately (as
//! [`RejectedAddon`]) so a future UI can surface them with reasons rather
//! than silently dropping them.
//!
//! No database, no caching layer. The filesystem is the source of truth;
//! `scan` is cheap enough to call on demand (rebuild after install/uninstall).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::compat::{ENGINE_API_VERSION, check_compat};
use super::error::{AddonError, Result};
use super::manifest::Manifest;

/// One successfully registered addon.
#[derive(Debug, Clone)]
#[allow(dead_code)] // `root`/`builtin` are informational metadata for a future UI
pub struct AddonEntry {
    pub manifest: Manifest,
    /// Absolute path to the addon directory (the one containing `manifest.toml`).
    /// Empty for builtin addons, which have no on-disk presence.
    pub root: PathBuf,
    /// Whether this addon ships inside the engine binary rather than on disk.
    /// The registry treats builtin and external addons identically once
    /// registered; this is purely informational (e.g. for a future UI).
    pub builtin: bool,
}

/// An addon directory that failed to load or validate. Kept so the UI can
/// show "addon X failed because Y" instead of silently dropping it.
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields surfaced via `rejected()` once the UI lists failures
pub struct RejectedAddon {
    pub root: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct AddonRegistry {
    entries: HashMap<String, AddonEntry>,
    rejected: Vec<RejectedAddon>,
    engine_api: u32,
}

impl AddonRegistry {
    pub fn new() -> Self {
        Self::with_engine_api(ENGINE_API_VERSION)
    }

    /// Useful for tests targeting specific API versions.
    pub fn with_engine_api(engine_api: u32) -> Self {
        Self {
            entries: HashMap::new(),
            rejected: Vec::new(),
            engine_api,
        }
    }

    /// Scan `root` for addons. Each direct subdirectory is treated as a
    /// potential addon; non-directories are ignored. A missing `root` is
    /// not an error — it yields an empty registry.
    ///
    /// `scan` is *additive*: existing valid entries are preserved if a
    /// rescan fails to re-read them. To start from scratch, call
    /// [`clear`](Self::clear) first.
    pub fn scan(&mut self, root: &Path) -> Result<()> {
        if !root.exists() {
            return Ok(());
        }
        if !root.is_dir() {
            return Err(AddonError::ManifestInvalid(format!(
                "addons root {root:?} is not a directory"
            )));
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if let Err(e) = self.load_one(&path) {
                eprintln!("[ADDON REJECTED]\npath={:?}\nreason={}\n", path, e);

                self.rejected.push(RejectedAddon {
                    root: path,
                    reason: e.to_string(),
                });
            }
        }
        Ok(())
    }

    fn load_one(&mut self, root: &Path) -> Result<()> {
        let manifest = Manifest::from_dir(root)?;
        check_compat(&manifest, self.engine_api)?;
        if self.entries.contains_key(&manifest.id) {
            return Err(AddonError::DuplicateAddon(manifest.id.clone()));
        }
        self.entries.insert(
            manifest.id.clone(),
            AddonEntry {
                manifest,
                root: root.to_owned(),
                builtin: false,
            },
        );
        Ok(())
    }

    /// Register an addon that ships inside the engine binary. The manifest is
    /// validated and compatibility-checked exactly like a scanned one, so the
    /// rest of the system (pipeline validation, params, UI) cannot tell a
    /// builtin from an external addon.
    pub fn register_builtin(&mut self, manifest: Manifest) -> Result<()> {
        manifest.validate()?;
        check_compat(&manifest, self.engine_api)?;
        if self.entries.contains_key(&manifest.id) {
            return Err(AddonError::DuplicateAddon(manifest.id.clone()));
        }
        self.entries.insert(
            manifest.id.clone(),
            AddonEntry {
                manifest,
                root: PathBuf::new(),
                builtin: true,
            },
        );
        Ok(())
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.rejected.clear();
    }

    pub fn get(&self, id: &str) -> Option<&AddonEntry> {
        self.entries.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &AddonEntry> {
        self.entries.values()
    }

    // The accessors below round out the registry's read surface. They are part
    // of the frozen public shape (used by tests today, and the natural API a
    // listing/diagnostics UI reaches for); `allow(dead_code)` keeps the build
    // clean until that UI wiring lands without trimming designed surface.

    #[allow(dead_code)]
    pub fn contains(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }

    /// Addons that failed to load/validate, kept so a UI can show "X failed
    /// because Y" instead of silently dropping them.
    #[allow(dead_code)]
    pub fn rejected(&self) -> &[RejectedAddon] {
        &self.rejected
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for AddonRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "static-addon-test-{}",
            nanoid::nanoid!(8, &nanoid::alphabet::SAFE)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_manifest(dir: &Path, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("manifest.toml"), body).unwrap();
    }

    fn good_manifest(id: &str, api_min: u32, api_max: u32) -> String {
        format!(
            r#"manifest_version = 1
id = "{id}"
name = "{id}"
version = "1.0.0"
author = "A"
api_min = {api_min}
api_max = {api_max}
kind = "pipeline"
"#
        )
    }

    #[test]
    fn scan_empty_root_is_ok() {
        let mut reg = AddonRegistry::new();
        reg.scan(Path::new("/this/path/does/not/exist")).unwrap();
        assert_eq!(reg.len(), 0);
        assert_eq!(reg.rejected().len(), 0);
    }

    #[test]
    fn scan_loads_valid_addon() {
        let root = tempdir();
        write_manifest(&root.join("crt"), &good_manifest("io.test.crt", 1, 1));

        let mut reg = AddonRegistry::new();
        reg.scan(&root).unwrap();
        assert_eq!(reg.len(), 1);
        assert!(reg.contains("io.test.crt"));
        assert_eq!(reg.rejected().len(), 0);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scan_rejects_incompatible_addon() {
        let root = tempdir();
        write_manifest(&root.join("future"), &good_manifest("io.test.future", 5, 9));

        let mut reg = AddonRegistry::new();
        reg.scan(&root).unwrap();
        assert_eq!(reg.len(), 0);
        assert_eq!(reg.rejected().len(), 1);
        assert!(reg.rejected()[0].reason.contains("incompatible API"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scan_rejects_malformed_manifest() {
        let root = tempdir();
        write_manifest(&root.join("bad"), "not valid toml at all }}}");

        let mut reg = AddonRegistry::new();
        reg.scan(&root).unwrap();
        assert_eq!(reg.len(), 0);
        assert_eq!(reg.rejected().len(), 1);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scan_handles_mixed_valid_and_invalid() {
        let root = tempdir();
        write_manifest(&root.join("ok"), &good_manifest("io.test.ok", 1, 1));
        write_manifest(&root.join("bad"), &good_manifest("io.test.bad", 9, 9));

        let mut reg = AddonRegistry::new();
        reg.scan(&root).unwrap();
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.rejected().len(), 1);

        fs::remove_dir_all(&root).ok();
    }
}
