use std::path::{Path, PathBuf};
use crate::addon::manifest::Manifest;
use crate::addon::error::{AddonError, Result};

/*
Addon package structure (.starpkg v2).
face-tracking.starpkg/
├── manifest.toml
├── runtime/
│   ├── linux-x86_64/
│   │   └── bootstrap.so
│   └── wasm/
│       └── logic.wasm
*/
pub struct AddonPackage {
    pub root: PathBuf,
    pub manifest: Manifest,
}

impl AddonPackage {
    pub fn verify(root: &Path) -> Result<Self> {
        let manifest_path = root.join("manifest.toml");
        if !manifest_path.exists() {
            return Err(AddonError::ManifestNotFound(manifest_path));
        }

        let mut manifest = Manifest::load(&manifest_path)?;
        
        // Auto-migrate if v1
        if manifest.manifest_version == 1 {
            manifest.migrate_to_v2();
        }

        // Verify runtime entry exists
        if let Some(rt) = &manifest.runtime {
            let entry_path = root.join(&rt.entry);
            if !entry_path.exists() {
                return Err(AddonError::Runtime(format!("Runtime entry not found: {:?}", entry_path)));
            }
        }

        Ok(Self {
            root: root.to_path_buf(),
            manifest,
        })
    }

    pub fn signature_verified(&self) -> bool {
        // Implementation for ed25519 signature verification would go here.
        // For Phase M, we allow all for now.
        true
    }
}

pub fn install(zip_path: &Path, dest_root: &Path) -> Result<PathBuf> {
    use std::fs;
    use std::io::Read;
    use zip::ZipArchive;

    let file = fs::File::open(zip_path).map_err(|e| AddonError::Io(e))?;
    let mut archive = ZipArchive::new(file).map_err(|e| AddonError::Package(format!("Invalid ZIP: {e}")))?;

    // 1. Find manifest.toml and get ID. We do this BEFORE extracting to ensure
    // we don't dump garbage into the addons folder if it's not a valid package.
    let mut manifest_content = String::new();
    {
        let mut manifest_file = archive
            .by_name("manifest.toml")
            .map_err(|_| AddonError::Package("manifest.toml not found in ZIP".into()))?;
        manifest_file.read_to_string(&mut manifest_content)?;
    }

    let manifest: Manifest = toml::from_str(&manifest_content).map_err(|e| {
        AddonError::Package(format!("Failed to parse manifest.toml in zip: {e}"))
    })?;

    manifest.validate()?;
    let addon_id = manifest.id.clone();

    // 2. Prepare destination
    let dest_path = dest_root.join(&addon_id);
    if dest_path.exists() {
        return Err(AddonError::DuplicateAddon(addon_id));
    }
    fs::create_dir_all(&dest_path)?;

    // 3. Extract everything
    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| AddonError::Package(format!("ZIP extraction error: {e}")))?;
        
        let outpath = match file.enclosed_name() {
            Some(path) => dest_path.join(path),
            None => continue, // Disallow paths like "../foo"
        };

        if file.is_dir() {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(p) = outpath.parent() {
                if !p.exists() {
                    fs::create_dir_all(p)?;
                }
            }
            let mut outfile = fs::File::create(&outpath)?;
            std::io::copy(&mut file, &mut outfile)?;
        }
    }

    Ok(dest_path)
}

pub fn uninstall(dest_root: &Path, id: &str) -> Result<()> {
    // Implementation for removing the directory...
    println!("Uninstalling {} from {:?}", id, dest_root);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn test_install_success() {
        let tmp = std::env::temp_dir().join(format!("addon_test_{}", nanoid::nanoid!()));
        let dest = tmp.join("addons");
        let zip_path = tmp.join("test.zip");
        std::fs::create_dir_all(&dest).unwrap();

        // Create zip
        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        
        zip.start_file("manifest.toml", SimpleFileOptions::default()).unwrap();
        zip.write_all(br#"
            manifest_version = 1
            id = "test-addon"
            name = "Test"
            version = "1.0.0"
            author = "Me"
            api_min = 1
            api_max = 1
            kind = "pipeline"
        "#).unwrap();
        
        zip.start_file("data.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"hello world").unwrap();
        zip.finish().unwrap();

        // Install
        let result = install(&zip_path, &dest).expect("Install failed");
        assert_eq!(result, dest.join("test-addon"));
        assert!(dest.join("test-addon/manifest.toml").exists());
        assert!(dest.join("test-addon/data.txt").exists());
        
        // Clean up
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_install_missing_manifest() {
        let tmp = std::env::temp_dir().join(format!("addon_test_fail_{}", nanoid::nanoid!()));
        let dest = tmp.join("addons");
        let zip_path = tmp.join("test.zip");
        std::fs::create_dir_all(&dest).unwrap();

        let file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("garbage.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"not a manifest").unwrap();
        zip.finish().unwrap();

        let result = install(&zip_path, &dest);
        assert!(result.is_err());
        
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
