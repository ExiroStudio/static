//! Error types for the addon ecosystem. One unified error covers manifest
//! parsing, registry scanning, compatibility checking, and pipeline config
//! handling — small enough to keep flat, structured enough to render useful
//! messages in a future UI.

use std::path::PathBuf;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, AddonError>;

#[derive(Debug, Error)]
pub enum AddonError {
    #[error("manifest not found at {0}")]
    ManifestNotFound(PathBuf),

    #[error("failed to read manifest {path}: {source}")]
    ManifestIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse manifest {path}: {source}")]
    ManifestParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("invalid manifest: {0}")]
    ManifestInvalid(String),

    #[error("incompatible API: engine={engine}, addon requires [{min}..={max}]")]
    IncompatibleApi { engine: u32, min: u32, max: u32 },

    #[error("duplicate addon id {0} (already registered)")]
    DuplicateAddon(String),

    #[error("addon not found: {0}")]
    NotFound(String),

    #[error("invalid pipeline config: {0}")]
    InvalidPipeline(String),

    /// A pipeline was structurally valid but failed validation against the
    /// registry (missing addons, bad params). Carries the rendered issue list.
    #[error("pipeline rejected:\n{0}")]
    PipelineRejected(String),

    /// An addon is installed (manifest present) but the runtime has no factory
    /// registered to instantiate it. In v1 every addon is builtin, so this only
    /// fires for a pipeline that references something the host can't construct.
    #[error("addon {0:?} has no runtime implementation registered")]
    NoImplementation(String),

    #[error("unsupported source type {0:?}")]
    UnsupportedSource(String),

    #[error("unsupported sink type {0:?}")]
    UnsupportedSink(String),

    #[error("package format error: {0}")]
    Package(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
