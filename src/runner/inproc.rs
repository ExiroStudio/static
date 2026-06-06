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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::addons::face_tracking_lite;
    use crate::behavior::builtins::time;
    use crate::behavior::node::Timing;
    use crate::runner::host::RecordingHost;
    use crate::runner::TickOutcome;
    use crate::signal::{SignalKind, SignalSchema, SignalValue};
    use std::sync::Arc;

    #[test]
    fn load_builds_unit_and_reports_publish_in_the_handshake() {
        let schema = Arc::new(SignalSchema::from_pairs(&[("signal.time", SignalKind::F32)]));
        let mut host = RecordingHost::with_schema(schema);

        let backend: Box<dyn RunnerBackend> =
            Box::new(InProcessRustRunner::new(time::init_with, "b", ParamMap::new(), true));
        let loaded = backend.load().unwrap();
        assert_eq!(loaded.kind(), RunnerKind::InProcessRust);
        assert_eq!(loaded.declares_publish(), &["signal.time".to_string()]);

        let bound = loaded.bind(&mut host);
        assert_eq!(bound.handshake().publish.len(), 1);
        assert_eq!(bound.handshake().version, crate::runner::ABI_VERSION);
    }

    #[test]
    fn drives_the_real_node_with_identical_output() {
        // The in-process runner reproduces the behavior's output exactly (the
        // node is unchanged; only the driver differs from the live scheduler).
        let schema = Arc::new(SignalSchema::from_pairs(&[("signal.time", SignalKind::F32)]));
        let mut host = RecordingHost::with_schema(schema).with_timing(Timing {
            dt: 0.0,
            elapsed: std::f32::consts::FRAC_PI_2,
        });
        let backend: Box<dyn RunnerBackend> =
            Box::new(InProcessRustRunner::new(time::init_with, "b", ParamMap::new(), true));
        let mut running = backend.load().unwrap().bind(&mut host).start(&mut host);
        let t = Timing {
            dt: 0.0,
            elapsed: std::f32::consts::FRAC_PI_2,
        };
        assert_eq!(running.tick(&mut host, t), TickOutcome::Ok);
        let (_slot, value) = *host.published.last().unwrap();
        match value {
            SignalValue::F32(x) => assert!((x - 1.0).abs() < 1e-5),
            other => panic!("{other:?}"),
        }
        let _ = running.stop(&mut host);
    }

    #[test]
    fn from_registry_bridges_the_existing_factory_seam() {
        let mut registry = BehaviorRegistry::new();
        registry.register("face-tracking-lite", face_tracking_lite::init_with);

        let node = NodeConfig {
            instance_id: "beh-face".into(),
            addon: "face-tracking-lite".into(),
            enabled: true,
            config: ParamMap::new(),
        };
        let runner = InProcessRustRunner::from_registry(&registry, &node).expect("registered");
        let loaded = Box::new(runner).load().unwrap();
        assert_eq!(
            loaded.declares_publish(),
            &[
                "face.position".to_string(),
                "face.rotation".to_string(),
                "face.scale".to_string()
            ]
        );

        let missing = NodeConfig {
            instance_id: "x".into(),
            addon: "nope".into(),
            enabled: true,
            config: ParamMap::new(),
        };
        assert!(InProcessRustRunner::from_registry(&registry, &missing).is_none());
    }
}
