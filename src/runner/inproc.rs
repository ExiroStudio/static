//! [`InProcessRustRunner`] — the (future-live) in-process backend.
//!
//! Refactored for Step-2 feedback: it no longer holds a `BehaviorNode` (F1) and
//! no longer carries a runtime state enum (F4). It is a [`RunnerBackend`] whose
//! `load` builds a [`BehaviorExecutionUnit`] (the node lives *there*) and enters
//! the typestate lifecycle. Still **dormant** — nothing in the live engine
//! constructs or drives it; behavior output is unchanged.

use crate::addon::pipeline::NodeConfig;
use crate::addon::schema::ParamMap;
use crate::behavior::{BehaviorFactory, BehaviorRegistry};

use super::backend::{LoadedRunner, RunnerBackend};
use super::execution::BehaviorExecutionUnit;
use super::{RunnerError, RunnerKind};

/// Binds a `BehaviorFactory` + per-instance config; builds the execution unit at
/// `load`.
pub struct InProcessRustRunner {
    factory: BehaviorFactory,
    instance_id: String,
    config: ParamMap,
    enabled: bool,
}

impl InProcessRustRunner {
    pub fn new(
        factory: BehaviorFactory,
        instance_id: impl Into<String>,
        config: ParamMap,
        enabled: bool,
    ) -> Self {
        Self {
            factory,
            instance_id: instance_id.into(),
            config,
            enabled,
        }
    }

    /// Compatibility adapter: resolve a `pipeline.json` behavior entry through the
    /// Phase-3a [`BehaviorRegistry`]. `None` if no factory is registered.
    pub fn from_registry(registry: &BehaviorRegistry, node: &NodeConfig) -> Option<Self> {
        registry.get(&node.addon).map(|factory| {
            Self::new(
                factory,
                node.instance_id.clone(),
                node.config.clone(),
                node.enabled,
            )
        })
    }
}

impl RunnerBackend for InProcessRustRunner {
    fn kind(&self) -> RunnerKind {
        RunnerKind::InProcessRust
    }

    fn load(self: Box<Self>) -> Result<LoadedRunner, RunnerError> {
        // Construct the node via the factory and hand it to the *unit* (F1) —
        // never store it in the runner.
        let init = (self.factory)(self.instance_id, self.config, self.enabled);
        let unit = BehaviorExecutionUnit::from_init(init);
        Ok(LoadedRunner::new(RunnerKind::InProcessRust, Box::new(unit)))
    }
}

// #[cfg(test)]
// mod tests {
//     ...
// }
