//! Addon package format — a plain ZIP archive.
//!
//! An addon package is an ordinary `.zip` (no custom extension — ZIP is
//! universally understood, easy to share, back up and install by hand). It
//! holds a single addon:
//!
//! ```text
//! crt.zip
//! ├── manifest.toml          required, at archive root
//! ├── icon.png               optional, used in UI listings
//! ├── README.md              optional
//! ├── LICENSE                optional
//! ├── shaders/               files referenced by [[shaders]] in manifest
//! │   └── crt.wgsl
//! ├── assets/                files referenced by [[assets]] in manifest
//! │   └── presets/soft.json
//! └── addon.wasm             optional, V2 only (scripted addons)
//! ```
//!
//! Installation extracts the archive into `<addons_root>/<manifest.id>/` after
//! validating its manifest; the registry then loads it on the next scan.
//! Uninstallation removes that directory.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use zip::ZipArchive;

use super::error::{AddonError, Result};
use super::manifest::{MANIFEST_FILENAME, Manifest};

/// Compute the destination directory an addon would extract into.
pub fn install_path(addons_root: &Path, id: &str) -> PathBuf {
    addons_root.join(id)
}

/// Read and validate the manifest inside a package without extracting the rest.
/// Used to learn the addon id (and reject bad packages) before touching disk.
pub fn peek_manifest(package_path: &Path) -> Result<Manifest> {
    let file = fs::File::open(package_path)?;
    let mut zip =
        ZipArchive::new(file).map_err(|e| AddonError::Package(format!("not a valid zip: {e}")))?;
    let manifest_entry = (0..zip.len()).find_map(|i| {
        let file = zip.by_index(i).ok()?;

        let name = file.name();

        if name == "manifest.toml" {
            Some(name.to_string())
        } else if name.ends_with("/manifest.toml") {
            Some(name.to_string())
        } else {
            None
        }
    });

    let Some(manifest_path) = manifest_entry else {
        return Err(AddonError::Package(format!(
            "{} not found in package",
            MANIFEST_FILENAME
        )));
    };

    eprintln!("[pkg] manifest={}", manifest_path);

    let mut entry = zip
        .by_name(&manifest_path)
        .map_err(|_| AddonError::Package(format!("failed opening {}", manifest_path)))?;
    let mut text = String::new();
    entry.read_to_string(&mut text)?;
    let manifest: Manifest = toml::from_str(&text).map_err(|e| AddonError::ManifestParse {
        path: package_path.into(),
        source: e,
    })?;
    manifest.validate()?;
    Ok(manifest)
}

/// Extract a ZIP package into `addons_root/<manifest.id>/`.
///
/// The manifest is validated first (this also yields the install id). An
/// existing install of the same id is replaced. Entries that would escape the
/// destination directory (zip-slip) are rejected. Returns the install path.
pub fn install(package_path: &Path, addons_root: &Path) -> Result<PathBuf> {
    eprintln!(
        "[pkg] install package={} root={}",
        package_path.display(),
        addons_root.display()
    );

    let manifest = peek_manifest(package_path)?;

    eprintln!("[pkg] manifest id={}", manifest.id);

    let dest = install_path(addons_root, &manifest.id);

    eprintln!("[pkg] dest={}", dest.display());

    // Fresh install: clear any previous version of this addon.
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::create_dir_all(&dest)?;

    let file = fs::File::open(package_path)?;
    let mut zip =
        ZipArchive::new(file).map_err(|e| AddonError::Package(format!("not a valid zip: {e}")))?;

    let root_prefix = (0..zip.len())
        .filter_map(|i| {
            zip.by_index(i)
                .ok()
                .and_then(|e| e.name().split('/').next().map(str::to_string))
        })
        .collect::<std::collections::HashSet<_>>();

    let strip_root = root_prefix.len() == 1;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| AddonError::Package(format!("corrupt zip entry: {e}")))?;
        eprintln!("[pkg] extract {}", entry.name());

        // `enclosed_name` returns `None` for unsafe paths (absolute, `..`, …).
        let Some(rel) = entry.enclosed_name() else {
            return Err(AddonError::Package(
                "package contains an unsafe path".into(),
            ));
        };

        // normalisasi supaya semua jadi PathBuf
        let rel: PathBuf = if strip_root {
            rel.strip_prefix(root_prefix.iter().next().unwrap())
                .ok()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or(rel)
        } else {
            rel
        };

        let out_path = dest.join(&rel);

        eprintln!("[pkg] {} -> {}", entry.name(), out_path.display());
        // Belt-and-suspenders against zip-slip.
        if !out_path.starts_with(&dest) {
            return Err(AddonError::Package(
                "package entry escapes the addon directory".into(),
            ));
        }

        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out)?;
        }
    }
    eprintln!("[pkg] install success {}", dest.display());
    Ok(dest)
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

    /// Build a minimal valid package zip in `path` with the given manifest body.
    fn write_package(path: &Path, manifest_body: &str, extra: &[(&str, &str)]) {
        use std::io::Write;
        use zip::write::{SimpleFileOptions, ZipWriter};
        let file = fs::File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("manifest.toml", opts).unwrap();
        zip.write_all(manifest_body.as_bytes()).unwrap();
        for (name, body) in extra {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    const PKG_MANIFEST: &str = r#"manifest_version = 1
id = "io.test.pkg"
name = "Packaged Addon"
version = "1.0.0"
author = "A"
api_min = 1
api_max = 1
kind = "pipeline"
"#;

    #[test]
    fn peek_reads_manifest_without_extracting() {
        let dir = std::env::temp_dir().join(format!(
            "static-pkg-peek-{}",
            nanoid::nanoid!(8, &nanoid::alphabet::SAFE)
        ));
        fs::create_dir_all(&dir).unwrap();
        let pkg = dir.join("addon.zip");
        write_package(&pkg, PKG_MANIFEST, &[]);

        let m = peek_manifest(&pkg).unwrap();
        assert_eq!(m.id, "io.test.pkg");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn install_extracts_into_id_directory() {
        let dir = std::env::temp_dir().join(format!(
            "static-pkg-install-{}",
            nanoid::nanoid!(8, &nanoid::alphabet::SAFE)
        ));
        let root = dir.join("addons");
        fs::create_dir_all(&root).unwrap();
        let pkg = dir.join("addon.zip");
        write_package(&pkg, PKG_MANIFEST, &[("shaders/main.wgsl", "// shader")]);

        let installed = install(&pkg, &root).unwrap();
        assert_eq!(installed, root.join("io.test.pkg"));
        assert!(installed.join("manifest.toml").exists());
        assert!(installed.join("shaders/main.wgsl").exists());
        // The installed directory loads as a registry addon.
        let m = Manifest::from_dir(&installed).unwrap();
        assert_eq!(m.id, "io.test.pkg");

        fs::remove_dir_all(&dir).ok();
    }

    /// End-to-end validation with the real `examples/glitch-monitor` addon:
    /// zip it, peek + install it, discover it via the registry, and confirm a
    /// pipeline referencing it passes registry validation (so the Add Addon
    /// dialog can list it and inserting it is valid). This exercises every step
    /// of the addon-management workflow that does *not* require a GPU.
    #[test]
    fn glitch_monitor_external_addon_end_to_end() {
        use crate::addon::pipeline::{PipelineConfig, SinkConfig, SourceConfig};
        use crate::addon::registry::AddonRegistry;

        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/glitch-monitor");
        if !src.exists() {
            return; // example sources not present; nothing to validate
        }

        let tmp = std::env::temp_dir().join(format!(
            "static-glitch-{}",
            nanoid::nanoid!(8, &nanoid::alphabet::SAFE)
        ));
        fs::create_dir_all(&tmp).unwrap();
        let zip_path = tmp.join("glitch-monitor.zip");
        zip_dir(&src, &zip_path);

        // Manifest validates and exposes its four schema params.
        let manifest = peek_manifest(&zip_path).unwrap();
        assert_eq!(manifest.id, "glitch-monitor");
        assert_eq!(manifest.params.len(), 4);

        // Install extracts into addons/<id>/ with the shader intact.
        let root = tmp.join("addons");
        fs::create_dir_all(&root).unwrap();
        let dest = install(&zip_path, &root).unwrap();
        assert_eq!(dest, root.join("glitch-monitor"));
        assert!(dest.join("shaders/glitch.wgsl").exists());
        assert!(dest.join("assets/presets/heavy.json").exists());

        // The registry discovers it on scan.
        let mut reg = AddonRegistry::new();
        reg.scan(&root).unwrap();
        assert!(reg.contains("glitch-monitor"));

        // A pipeline referencing it passes registry validation — the Add Addon
        // dialog lists it and inserting it produces a structurally valid config.
        let mut pipeline = PipelineConfig::new(
            SourceConfig {
                kind: "webcam".into(),
                config: serde_json::Value::Object(Default::default()),
            },
            SinkConfig {
                kind: "window".into(),
                config: serde_json::Value::Object(Default::default()),
            },
        );
        pipeline.add_node("glitch-monitor", None);
        assert!(pipeline.validate_against(&reg).is_empty());

        // Uninstall removes the directory; a rescan no longer finds it.
        uninstall(&root, "glitch-monitor").unwrap();
        let mut reg2 = AddonRegistry::new();
        reg2.scan(&root).unwrap();
        assert!(!reg2.contains("glitch-monitor"));

        fs::remove_dir_all(&tmp).ok();
    }

    /// Recursively zip a directory so that its contents sit at the archive root.
    fn zip_dir(src: &Path, out: &Path) {
        use std::io::Write;
        use zip::write::{SimpleFileOptions, ZipWriter};

        fn add(zip: &mut ZipWriter<fs::File>, base: &Path, dir: &Path, opts: SimpleFileOptions) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if path.is_dir() {
                    add(zip, base, &path, opts);
                } else {
                    zip.start_file(rel, opts).unwrap();
                    zip.write_all(&fs::read(&path).unwrap()).unwrap();
                }
            }
        }

        let file = fs::File::create(out).unwrap();
        let mut zip = ZipWriter::new(file);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        add(&mut zip, src, src, opts);
        zip.finish().unwrap();
    }

    #[test]
    fn install_rejects_zip_without_manifest() {
        let dir = std::env::temp_dir().join(format!(
            "static-pkg-bad-{}",
            nanoid::nanoid!(8, &nanoid::alphabet::SAFE)
        ));
        let root = dir.join("addons");
        fs::create_dir_all(&root).unwrap();
        let pkg = dir.join("addon.zip");
        {
            use std::io::Write;
            use zip::write::{SimpleFileOptions, ZipWriter};
            let file = fs::File::create(&pkg).unwrap();
            let mut zip = ZipWriter::new(file);
            zip.start_file("readme.txt", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"no manifest here").unwrap();
            zip.finish().unwrap();
        }
        assert!(install(&pkg, &root).is_err());
        fs::remove_dir_all(&dir).ok();
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
