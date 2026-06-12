// [`Sandbox`] — the confinement seam. **Trait + placeholders only.**
//
// Step 1 defines the surface and three platform stubs; it implements **no**
// enforcement (no seccomp/Landlock/namespaces/AppContainer/`sandbox_init`). Every
// backend's [`apply`](Sandbox::apply) returns [`SandboxError::NotImplemented`] so
// the absence of confinement is explicit and can never be mistaken for a granted
// enforcement (no seccomp/Landlock/namespaces/AppContainer/`sandbox_init`). Every
// backend's [`apply`](Sandbox::apply) returns [`SandboxError::NotImplemented`] so
// the absence of confinement is explicit and can never be mistaken for a granted
// sandbox. Real enforcement is a later step (Linux first, per the RFC).

#[path = "sandbox_linux.rs"]
mod sandbox_linux;
pub use sandbox_linux::LinuxLandlockSandbox;

// [`SandboxSpec`] carries the *requested* capabilities (a declaration, not
// enforcement). `gpu_compute` is the addon's own GPU-compute context (never the
// engine's GPU) per RFC §Q4a.

/// The capabilities a runner *requests* for its child. Step 1: a declaration the
/// trait would consume — nothing here is enforced.
#[derive(Debug, Clone, Copy, Default)]
pub struct SandboxSpec {
    pub network: bool,
    pub filesystem: bool,
    pub camera: bool,
    /// Full-resolution frames (system tier only).
    pub frame_fullres: bool,
    /// Independent GPU *compute* context in the addon's own process (CUDA/Vulkan/
    /// etc.). Never exposes the engine's GPU. Default-deny.
    pub gpu_compute: bool,
    pub spawn_worker: bool,
}

/// Why a sandbox could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// No enforcement backend on this platform yet (Step 1 everywhere).
    NotImplemented,
    /// The platform cannot honor part of the spec (reserved for real backends).
    Unsupported(&'static str),
}

/// Applies OS confinement to a child process before it runs untrusted code.
pub trait Sandbox {
    /// Confine according to `spec`. Step 1 backends return
    /// [`SandboxError::NotImplemented`].
    fn apply(&self, spec: &SandboxSpec) -> Result<(), SandboxError>;

    /// Human-readable platform tag (diagnostics).
    fn platform(&self) -> &'static str;
}

/// Linux confinement (future: seccomp-bpf + Landlock + namespaces + cgroups).
#[derive(Debug, Default)]
pub struct LinuxSandbox {
    inner: LinuxLandlockSandbox,
}
impl Sandbox for LinuxSandbox {
    fn apply(&self, spec: &SandboxSpec) -> Result<(), SandboxError> {
        self.inner.apply(spec)
    }
    fn platform(&self) -> &'static str {
        "linux"
    }
}

/// macOS confinement (future: App Sandbox / `sandbox_init` profile — coarser).
#[derive(Debug, Default)]
pub struct MacSandbox;
impl Sandbox for MacSandbox {
    fn apply(&self, _spec: &SandboxSpec) -> Result<(), SandboxError> {
        Err(SandboxError::NotImplemented)
    }
    fn platform(&self) -> &'static str {
        "macos"
    }
}

/// Windows confinement (future: AppContainer + Job Object + restricted token).
#[derive(Debug, Default)]
pub struct WindowsSandbox;
impl Sandbox for WindowsSandbox {
    fn apply(&self, _spec: &SandboxSpec) -> Result<(), SandboxError> {
        Err(SandboxError::NotImplemented)
    }
    fn platform(&self) -> &'static str {
        "windows"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_report_platform_and_enforce_nothing() {
        let backends: [(&dyn Sandbox, &str); 3] = [
            (&LinuxSandbox, "linux"),
            (&MacSandbox, "macos"),
            (&WindowsSandbox, "windows"),
        ];
        let spec = SandboxSpec {
            camera: true,
            gpu_compute: true,
            ..SandboxSpec::default()
        };
        for (sb, name) in backends {
            assert_eq!(sb.platform(), name);
            assert_eq!(
                sb.apply(&spec),
                Err(SandboxError::NotImplemented),
                "Step 1 enforces nothing — and must say so, not silently 'allow'"
            );
        }
    }

    #[test]
    fn spec_defaults_to_all_denied() {
        let s = SandboxSpec::default();
        assert!(!s.network && !s.filesystem && !s.camera);
        assert!(!s.frame_fullres && !s.gpu_compute && !s.spawn_worker);
    }
}
