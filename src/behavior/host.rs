//! Behavior host seam — the Phase 3 execution path for *external* behavior addons.
//!
//! Builtins were dispatched by a hardcoded `match` on the addon id, which coupled
//! every new producer to an engine edit. The host removes that coupling:
//!
//! ```text
//!   manifest (on disk)  ──register_behavior_with──▶  BehaviorRegistry
//!   pipeline.json entry ──BehaviorHost::create_inits──▶  BehaviorInit ──▶ scheduler
//! ```
//!
//! A behavior addon registers a [`BehaviorFactory`] under its id; the engine
//! creates instances by **registry lookup**, never by name. The factory's code
//! is compiled in (v1 loads no scripting/native/wasm — those remain Phase 3b),
//! but it is bound to an id-addressed package whose manifest/config live on disk
//! like any addon, so adding a producer never again touches the dispatch.
//!
//! This is the *only* new execution surface: `BehaviorNode`, the scheduler, the
//! store, and the render path are unchanged.

use std::collections::HashMap;

use crate::addon::pipeline::NodeConfig;
use crate::addon::schema::ParamMap;

use super::BehaviorInit;

/// Constructs a runnable behavior instance for one pipeline entry — the same
/// shape as the existing `init_with` constructors, now addressed by id instead
/// of a `match`. Signature: `(instance_id, config values, enabled) -> BehaviorInit`.
pub type BehaviorFactory = fn(String, ParamMap, bool) -> BehaviorInit;

/// Maps a behavior addon id → the factory that builds its instances. This is the
/// single source of "can this behavior execute?": registration is the only way
/// in, replacing the former hardcoded dispatch arm.
#[derive(Default)]
pub struct BehaviorRegistry {
    factories: HashMap<String, BehaviorFactory>,
}

impl BehaviorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `id` to `factory`. Last registration wins, so a compiled factory can
    /// attach to an id already provided by a scanned on-disk package.
    pub fn register(&mut self, id: &str, factory: BehaviorFactory) {
        self.factories.insert(id.to_owned(), factory);
    }

    /// The factory for `id`, if any was registered. `fn` pointers are `Copy`.
    pub fn get(&self, id: &str) -> Option<BehaviorFactory> {
        self.factories.get(id).copied()
    }

    #[allow(dead_code)] // read surface used by tests + a future "executable?" UI badge
    pub fn contains(&self, id: &str) -> bool {
        self.factories.contains_key(id)
    }

    #[allow(dead_code)] // read surface for diagnostics / a future "executable?" UI badge
    pub fn len(&self) -> usize {
        self.factories.len()
    }
}

/// Turns a config's behavior set into runnable inits by **registry lookup**.
///
/// A behavior whose id has no registered factory is skipped (it is still listed
/// and schema-validated elsewhere — it simply cannot execute). This is the same
/// tolerance the old `match` had for unknown ids, minus the per-addon arm.
pub struct BehaviorHost;

impl BehaviorHost {
    pub fn create_inits(
        registry: &BehaviorRegistry,
        behaviors: &[NodeConfig],
    ) -> (Vec<BehaviorInit>, Vec<(String, super::SkipReason)>) {
        let mut inits = Vec::new();
        let mut skipped = Vec::new();

        for node in behaviors {
            match registry.get(&node.addon) {
                Some(make) => inits.push(make(
                    node.instance_id.clone(),
                    node.config.clone(),
                    node.enabled,
                )),
                None => {
                    eprintln!(
                        "[engine] behavior addon {:?} has no registered factory — skipped",
                        node.addon
                    );
                    skipped.push((node.addon.clone(), super::SkipReason::FilesystemMissing));
                }
            }
        }
        (inits, skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::schema::ParamMap;
    use crate::behavior::node::{BehaviorCtx, BehaviorNode, BehaviorStartCtx};
    use crate::behavior::SkipReason;
    use crate::signal::SignalSpec;
    use std::collections::BTreeMap;

    /// A do-nothing behavior used purely to prove the factory→init→lookup wiring.
    struct Noop;
    impl BehaviorNode for Noop {
        fn start(&mut self, _: &mut BehaviorStartCtx) {}
        fn update(&mut self, _: &mut BehaviorCtx) {}
        fn stop(&mut self) {}
    }

    /// A `BehaviorFactory` — note this is a plain `fn`, coercible to the alias.
    fn make_noop(instance_id: String, values: ParamMap, enabled: bool) -> BehaviorInit {
        BehaviorInit {
            instance_id,
            node: Box::new(Noop),
            publish: vec![SignalSpec {
                name: "noop.signal".into(),
                kind: crate::signal::SignalKind::F32,
            }],
            specs: BTreeMap::new(),
            values,
            enabled,
        }
    }

    fn node(addon: &str, id: &str) -> NodeConfig {
        NodeConfig {
            instance_id: id.into(),
            addon: addon.into(),
            enabled: true,
            config: ParamMap::new(),
        }
    }

    #[test]
    fn registry_register_and_lookup() {
        let mut reg = BehaviorRegistry::new();
        assert!(!reg.contains("noop"));
        reg.register("noop", make_noop);
        assert!(reg.contains("noop"));
        assert!(reg.get("noop").is_some());
        assert!(reg.get("missing").is_none());
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn host_creates_inits_for_registered_ids_only() {
        let mut reg = BehaviorRegistry::new();
        reg.register("noop", make_noop);

        // Two entries: one registered, one not. Only the registered one executes.
        let behaviors = vec![node("noop", "a"), node("unregistered", "b")];
        let (inits, skipped) = BehaviorHost::create_inits(&reg, &behaviors);

        assert_eq!(inits.len(), 1, "unregistered behavior is skipped, not faked");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].1, SkipReason::FilesystemMissing);
        assert_eq!(inits[0].instance_id, "a");
        assert_eq!(inits[0].publish.len(), 1);
        assert_eq!(inits[0].publish[0].name, "noop.signal");
    }

    #[test]
    fn host_preserves_per_instance_config_and_enabled() {
        let mut reg = BehaviorRegistry::new();
        reg.register("noop", make_noop);
        let mut n = node("noop", "x");
        n.enabled = false;
        let (inits, _) = BehaviorHost::create_inits(&reg, &[n]);
        assert_eq!(inits.len(), 1);
        assert!(!inits[0].enabled, "disabled flag flows through the factory");
    }
}
