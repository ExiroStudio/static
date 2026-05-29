//! Addon package format (`.starpkg`).
//!
//! A `.starpkg` is a zip archive holding a single addon. Filename convention:
//!
//! ```text
//! <addon_id>-<version>.starpkg
//! ```
//!
//! Archive layout (everything except `manifest.toml` is optional and only
//! loaded if referenced by the manifest):
//!
//! ```text
//! io.static.crt-1.0.0.starpkg
//! ├── manifest.toml          required, at archive root
//! ├── icon.png               optional, used in UI listings
//! ├── preview.png            optional, used by marketplace
//! ├── README.md              optional
//! ├── LICENSE                optional
//! ├── shaders/               files referenced by [[shaders]] in manifest
//! │   └── crt.wgsl
//! ├── assets/                files referenced by [[assets]] in manifest
//! │   └── presets/soft.json
//! └── addon.wasm             optional, V2 only (scripted addons)
//! ```
//!
//! Installation is the extraction of the archive into
//! `<addons_root>/<manifest.id>/`. The registry then loads it on the next
//! scan. Uninstallation is the removal of that directory.
//!
//! v1 ships uninstall and the on-disk format. Archive read/write (peek,
//! install) is deferred to v1.1 — until then, users drop pre-extracted
//! addon directories into `<addons_root>/` by hand (or via tooling).

use std::fs;
use std::path::{Path, PathBuf};

use super::error::{AddonError, Result};
use super::manifest::Manifest;

pub const PACKAGE_EXTENSION: &str = "starpkg";

/// Build the canonical filename for an addon package.
pub fn package_filename(id: &str, version: &str) -> String {
    format!("{id}-{version}.{PACKAGE_EXTENSION}")
}

/// Compute the destination directory an addon would extract into.
pub fn install_path(addons_root: &Path, id: &str) -> PathBuf {
    addons_root.join(id)
}

/// Read a manifest from an already-extracted addon directory. Convenience
/// wrapper over [`Manifest::from_dir`] kept here so callers can reach for
/// "package operations" in one module.
pub fn manifest_from_dir(dir: &Path) -> Result<Manifest> {
    Manifest::from_dir(dir)
}

/// Extract a `.starpkg` package into `addons_root/<id>/`.
///
/// **v1 status:** not yet implemented. The supporting zip dependency lands
/// in v1.1. For now, users install addons by placing pre-extracted directories
/// into the addons root.
pub fn install(_package_path: &Path, _addons_root: &Path) -> Result<PathBuf> {
    Err(AddonError::Package(
        "package install requires the zip backend (planned for v1.1); \
         drop extracted addon directories into the addons root instead"
            .into(),
    ))
}

/// Peek at the manifest inside a `.starpkg` without extracting the rest.
///
/// **v1 status:** not yet implemented (see [`install`]).
pub fn peek_manifest(_package_path: &Path) -> Result<Manifest> {
    Err(AddonError::Package(
        "package peek requires the zip backend (planned for v1.1)".into(),
    ))
}

/// Remove an installed addon's directory by id.
pub fn uninstall(addons_root: &Path, id: &str) -> Result<()> {
    let path = install_path(addons_root, id);
    if path.exists() {
        fs::remove_dir_all(&path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_convention() {
        assert_eq!(
            package_filename("io.static.crt", "1.0.0"),
            "io.static.crt-1.0.0.starpkg"
        );
    }

    #[test]
    fn install_path_joins_id() {
        let p = install_path(Path::new("/addons"), "io.static.crt");
        assert_eq!(p, PathBuf::from("/addons/io.static.crt"));
    }

    #[test]
    fn uninstall_missing_is_ok() {
        // Removing a non-existent addon should be a no-op, not an error.
        let root = std::env::temp_dir().join(format!(
            "static-uninstall-test-{}",
            nanoid::nanoid!(8, &nanoid::alphabet::SAFE)
        ));
        fs::create_dir_all(&root).unwrap();
        uninstall(&root, "does.not.exist").unwrap();
        fs::remove_dir_all(&root).ok();
    }
}
