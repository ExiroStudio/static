//! API compatibility validation between engine and addon.
//!
//! Each addon declares the range of engine API versions it works against
//! (`api_min`..=`api_max`). The host bumps [`ENGINE_API_VERSION`] only on
//! breaking changes to the runtime↔addon contract. Within a major API
//! version, only additive changes are permitted (new signal schemas, new
//! host functions, new optional manifest fields).

use super::error::{AddonError, Result};
use super::manifest::Manifest;

/// Current host API version. Bumped on breaking addon-contract changes.
pub const ENGINE_API_VERSION: u32 = 1;

/// Returns `Ok(())` if `engine_api` falls within `[manifest.api_min,
/// manifest.api_max]`. Otherwise returns [`AddonError::IncompatibleApi`]
/// with the gap reported.
pub fn check_compat(manifest: &Manifest, engine_api: u32) -> Result<()> {
    if engine_api < manifest.api_min || engine_api > manifest.api_max {
        return Err(AddonError::IncompatibleApi {
            engine: engine_api,
            min: manifest.api_min,
            max: manifest.api_max,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::manifest::AddonKind;

    fn manifest(min: u32, max: u32) -> Manifest {
        Manifest {
            manifest_version: 1,
            id: "x".into(),
            name: "X".into(),
            version: "1.0".into(),
            author: "A".into(),
            description: String::new(),
            license: None,
            homepage: None,
            tags: vec![],
            api_min: min,
            api_max: max,
            kind: AddonKind::Pipeline,
            runner: None,
            entry: None,
            permissions: Default::default(),
            shaders: vec![],
            assets: vec![],
            params: Default::default(),
            publish: vec![],
            consume: vec![],
            pipeline: None,
        }
    }

    #[test]
    fn accepts_in_range() {
        assert!(check_compat(&manifest(1, 2), 1).is_ok());
        assert!(check_compat(&manifest(1, 2), 2).is_ok());
    }

    #[test]
    fn rejects_too_old_engine() {
        let err = check_compat(&manifest(3, 5), 1).unwrap_err();
        assert!(matches!(
            err,
            AddonError::IncompatibleApi {
                engine: 1,
                min: 3,
                max: 5
            }
        ));
    }

    #[test]
    fn rejects_too_new_engine() {
        let err = check_compat(&manifest(1, 1), 2).unwrap_err();
        assert!(matches!(
            err,
            AddonError::IncompatibleApi {
                engine: 2,
                min: 1,
                max: 1
            }
        ));
    }
}
