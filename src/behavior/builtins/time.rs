//! TimeBehavior — the one builtin producer: publishes `signal.time = sin(t)`.
//!
//! It reads no frame and holds no resources; it exists to prove the
//! behavior→signal path end-to-end (the CRT filter consumes `signal.time`).

use std::collections::BTreeMap;

use crate::addon::manifest::{AddonKind, Manifest, CURRENT_MANIFEST_VERSION};
use crate::addon::schema::ParamMap;
use crate::behavior::node::{BehaviorCtx, BehaviorNode, BehaviorStartCtx};
use crate::behavior::BehaviorInit;
use crate::signal::{SignalId, SignalValue};

/// The signal this behavior produces.
const TIME_SIGNAL: &str = "signal.time";

#[derive(Default)]
pub struct TimeBehavior {
    /// Resolved once in `start`; `None` if the schema lacks `signal.time`.
    time_id: Option<SignalId>,
}

impl BehaviorNode for TimeBehavior {
    fn start(&mut self, ctx: &mut BehaviorStartCtx) {
        self.time_id = ctx.schema().id(TIME_SIGNAL);
    }

    fn update(&mut self, ctx: &mut BehaviorCtx) {
        if let Some(id) = self.time_id {
            let t = ctx.timing().elapsed;
            ctx.publish(id, SignalValue::F32(t.sin()));
        }
    }

    fn stop(&mut self) {
        self.time_id = None;
    }
}

/// The behavior's manifest (`kind = behavior`). Declared for forward
/// compatibility with the registry/UI; the runtime constructs the instance
/// directly via [`init`].
pub fn manifest() -> Manifest {
    Manifest {
        manifest_version: CURRENT_MANIFEST_VERSION,
        id: "time".into(),
        name: "Time".into(),
        version: "1.0.0".into(),
        author: "static (builtin)".into(),
        description: "Publishes signal.time = sin(elapsed).".into(),
        license: None,
        homepage: None,
        tags: vec!["behavior".into()],
        api_min: 1,
        api_max: 1,
        kind: AddonKind::Behavior,
        permissions: Default::default(),
        shaders: vec![],
        assets: vec![],
        params: BTreeMap::new(),
    }
}

/// Construct the runnable behavior instance the engine seeds the runtime with.
pub fn init() -> BehaviorInit {
    BehaviorInit {
        instance_id: "time".into(),
        node: Box::new(TimeBehavior::default()),
        specs: BTreeMap::new(),
        values: ParamMap::new(),
        enabled: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_a_valid_behavior() {
        let m = manifest();
        assert_eq!(m.kind, AddonKind::Behavior);
        m.validate().expect("time behavior manifest must validate");
    }
}
