use async_trait::async_trait;
use crate::runner::backend::{LoadedRunner, RunnerBackend};
use crate::runner::execution::ExecutionUnit;
use crate::runner::host::HostApi;
use crate::runner::{Capabilities, RunnerError, RunnerKind, TickOutcome, Handshake};
use crate::behavior::node::Timing;
use crate::runtime::driver::DriverBox;
use crate::addon::error::{AddonError, Result};

/// A universal backend that delegates all execution to a `RuntimeDriver`.
/// This replaces the hardcoded NativeRunnerBackend/WasmRunnerBackend etc.
pub struct UniversalRunnerBackend {
    driver: DriverBox,
    entry: String,
    kind: RunnerKind,
}

impl UniversalRunnerBackend {
    pub fn new(driver: DriverBox, entry: String) -> Self {
        Self {
            driver,
            entry,
            kind: RunnerKind::Native, // Default; should be refined based on driver.kind()
        }
    }
}

impl RunnerBackend for UniversalRunnerBackend {
    fn kind(&self) -> RunnerKind {
        self.kind
    }

    fn load(self: Box<Self>) -> std::result::Result<LoadedRunner, RunnerError> {
        // Since load is async on RuntimeDriver but sync on RunnerBackend,
        // we might need a block_on or refactor. For this demo, we assume
        // we can bridge it.
        let mut driver = self.driver;
        let entry = self.entry.clone();
        
        // This is a bridge: in a real production system, the entire stack might be async
        pollster::block_on(driver.load(&entry))
            .map_err(|e| RunnerError::Load(format!("Driver load failed: {}", e)))?;

        let unit = UniversalExecutionUnit { driver };
        Ok(LoadedRunner::new(self.kind, Box::new(unit)))
    }
}

struct UniversalExecutionUnit {
    driver: DriverBox,
}

impl ExecutionUnit for UniversalExecutionUnit {
    fn publishes(&self) -> &[String] {
        &[] // Resolved at bind time via handshake
    }

    fn consumes(&self) -> &[String] {
        &[]
    }

    fn caps(&self) -> Capabilities {
        Capabilities::default() // Negotiated via PolicyEngine earlier
    }

    fn start(&mut self, host: &mut dyn HostApi) {
        pollster::block_on(self.driver.start(host)).unwrap();
    }

    fn run(&mut self, host: &mut dyn HostApi, timing: Timing) -> TickOutcome {
        pollster::block_on(self.driver.tick(host, timing))
    }

    fn stop(&mut self, host: &mut dyn HostApi) {
        pollster::block_on(self.driver.stop(host)).unwrap();
    }
}
