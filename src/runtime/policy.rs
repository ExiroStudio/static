use crate::addon::manifest::{PermissionsV2, SandboxSpec};
use crate::runner::Capabilities;

/// Unified policy engine for capability negotiation and sandbox enforcement.
pub struct PolicyEngine;

impl PolicyEngine {
    /// Negotiate requested permissions into effective runtime capabilities.
    pub fn negotiate(requested: &PermissionsV2) -> Capabilities {
        Capabilities {
            network: !requested.network.is_empty(),
            filesystem: true, // Internal filesystem always allowed
            camera: requested.camera,
            frame_fullres: false, // Restricted to system tier
            gpu_compute: matches!(requested.gpu, crate::addon::manifest::GpuPerm::Extended),
            spawn_worker: false,  // Restricted by default
        }
    }

    /// Resolve a manifest's sandbox spec into a concrete platform sandbox profile.
    pub fn resolve_sandbox(spec: &SandboxSpec) -> crate::runner::SandboxSpec {
        crate::runner::SandboxSpec {
            network: spec.network,
            filesystem: spec.filesystem != "none",
            camera: true, // Inherited from permissions
            frame_fullres: false,
            gpu_compute: true,
            spawn_worker: false,
        }
    }
}
