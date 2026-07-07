/// Represents a compiler-assigned stable runtime handle for a graph node instance.
/// The underlying representation is opaque and must not be relied upon.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct NodeHandle(u32);

impl NodeHandle {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Represents a compiler-generated semantic handle for an artifact output slot.
/// ArtifactHandle is a completely opaque compiler-generated runtime identity.
/// Addons must only consume SDK-defined constants (e.g. `ARTIFACT_INSTANCES`)
/// and must never attempt to generate, compute, or mutate runtime handles.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct ArtifactHandle(u32);

impl ArtifactHandle {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// SDK-defined constants for common artifacts. Addons must only consume these
/// predefined handles rather than attempting to construct or compute their own.
pub const ARTIFACT_INSTANCES: ArtifactHandle = ArtifactHandle::new(105446707); // FNV-1a hash of "instances"
pub const ARTIFACT_VERTICES: ArtifactHandle = ArtifactHandle::new(2082523534);  // FNV-1a hash of "vertices"
pub const ARTIFACT_ATLAS: ArtifactHandle = ArtifactHandle::new(3407328204);    // FNV-1a hash of "atlas"

/// Represents a versioned schema layout handle.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct SchemaHandle(u32);

impl SchemaHandle {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// Computes a stable 32-bit FNV-1a hash of a string at compile-time.
pub const fn hash_str(s: &str) -> u32 {
    let mut hash = 2166136261u32;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u32;
        hash = hash.wrapping_mul(16777619);
        i += 1;
    }
    hash
}

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

pub static CURRENT_FRAME: AtomicU64 = AtomicU64::new(0);
pub static ENGINE_START: OnceLock<Instant> = OnceLock::new();

pub fn relative_time() -> f32 {
    let start = ENGINE_START.get_or_init(Instant::now);
    start.elapsed().as_secs_f32()
}

#[macro_export]
macro_rules! trace_log {
    ($subsystem:expr, $($arg:tt)*) => {{
        let msg = format!($($arg)*);
        let frame = $crate::runtime::CURRENT_FRAME.load(std::sync::atomic::Ordering::Relaxed);
        let time = $crate::runtime::relative_time();
        println!("[FRAME {}] [{:.6}s] [{}] {}", frame, time, $subsystem, msg);
    }};
}

