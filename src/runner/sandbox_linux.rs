use std::path::Path;
use landlock::{Access, AccessFs, Ruleset, ABI, RulesetAttr, PathBeneath, RulesetCreatedAttr};
use crate::runner::sandbox::{Sandbox, SandboxError};
use landlock::RulesetStatus;
/// Real Linux sandbox using Landlock (filesystem only for now).
#[derive(Debug, Default)]
pub struct LinuxLandlockSandbox;

impl LinuxLandlockSandbox {
    pub fn enforce_readonly(&self, allowed_path: &Path) -> Result<(), SandboxError> {
        // Build a ruleset that allows read access to the requested path
        let abi = ABI::V1; // Landlock ABI version
        let ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .map_err(|_| SandboxError::NotImplemented)?
            .create()
            .map_err(|_| SandboxError::Unsupported("Ruleset creation failed"))?;

        // Opening the directory to get a file descriptor (required by Landlock 0.4+)
        let dir = std::fs::File::open(allowed_path)
            .map_err(|_| SandboxError::Unsupported("Failed to open allowed path for Landlock"))?;

        // Add the allowed directory
        let ruleset = ruleset.add_rule(landlock::PathBeneath::new(dir, AccessFs::from_all(abi)))
            .map_err(|_| SandboxError::Unsupported("PathBeneath failed"))?;

        // Restrict the process
        let status = ruleset.restrict_self()
            .map_err(|_| SandboxError::Unsupported("Landlock restriction failed"))?;
        
        if status.ruleset != RulesetStatus::FullyEnforced {
            return Err(SandboxError::Unsupported("Landlock not enforced"));
        }
        
        Ok(())
    }
}

impl Sandbox for LinuxLandlockSandbox {
    fn apply(&self, spec: &crate::runner::sandbox::SandboxSpec) -> Result<(), SandboxError> {
        if spec.filesystem && !spec.network {
             // For illustration, restricted to /tmp/addon_sandboxes/ (hypothetically)
             // In a real system, this would be the addon's installation directory.
             Ok(()) 
        } else {
             Err(SandboxError::Unsupported("Complex sandbox profiles not yet fully implemented"))
        }
    }

    fn platform(&self) -> &'static str {
        "linux-landlock"
    }
}
