use async_trait::async_trait;
use crate::runner::{TickOutcome, Handshake, HostApi};
use crate::behavior::node::Timing;
use crate::addon::error::Result;

/// The core abstraction for all addon execution backends.
/// Every runner (Native, WASM, JS, etc.) must implement this trait.
#[async_trait]
pub trait RuntimeDriver: Send + Sync {
    /// Unique identifier for this runtime type (e.g., "native", "wasm").
    fn kind(&self) -> &'static str;

    /// Prepare the runtime: allocation, setup, but no execution.
    async fn load(&mut self, entry_point: &str) -> Result<()>;

    /// Bind the host API and perform the handshake.
    async fn bind(&mut self, host: &mut dyn HostApi) -> Result<Handshake>;

    /// Transition to active execution.
    async fn start(&mut self, host: &mut dyn HostApi) -> Result<()>;

    /// Execute one step of work.
    async fn tick(&mut self, host: &mut dyn HostApi, timing: Timing) -> TickOutcome;

    /// Graceful shutdown and resource release.
    async fn stop(&mut self, host: &mut dyn HostApi) -> Result<()>;
}

/// A handle to a driver instance.
pub type DriverBox = Box<dyn RuntimeDriver>;
