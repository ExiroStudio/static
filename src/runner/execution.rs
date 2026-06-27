//! [`ExecutionUnit`] — the neutral work abstraction (Step-1 feedback F1).
//!
//! A runner must **not** hold a `BehaviorNode` directly: a future native runner
//! has no node, only a process. So the runner owns a `Box<dyn ExecutionUnit>` and
//! the unit owns the actual work. Two units exist in Step 2, both in-process and
//! test-only (the runner is still dormant):
//!
//! * [`BehaviorExecutionUnit`] — wraps a real `BehaviorNode` (from a
//!   `BehaviorFactory`) and drives `node.update()` through a [`HostApi`], proving
//!   the `Factory → ExecutionUnit → HostApi → BehaviorNode.update()` seam.
//! * [`MockExecutionUnit`] — a controllable fake (records calls, can publish a
//!   canned value, can fault on demand) for exercising the runner/supervisor/
//!   host wiring without a real behavior.
//!
//! Driving a node needs a `SignalPublisher` (the node only speaks `BehaviorCtx`).
//! The behavior unit therefore owns a private, single-node `SignalStore` as a
//! translation buffer and mirrors what the node publishes onto the host — it
//! **uses** the signal types, it does not modify them, and it never touches the
//! live scheduler.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::addon::schema::{ParamMap, ParamSpec};
use crate::behavior::node::{BehaviorCtx, BehaviorNode, BehaviorStartCtx, Timing};
use crate::behavior::BehaviorInit;
use crate::runtime::ResolvedConfig;
use crate::signal::{
    SignalReader, SignalSchema, SignalSchemaBuilder, SignalSnapshot, SignalStore, SignalValue,
};

use super::host::{FrameTier, HostApi};
use super::{Capabilities, TickOutcome};

/// The work behind a runner. Neutral: no `BehaviorNode` appears in this surface,
/// so a native (process-backed) unit can implement it identically later.
pub trait ExecutionUnit {
    /// Signal names this unit publishes (internal declaration; resolved to ids at
    /// bind to build the tiny `Handshake`). Not part of the ABI.
    fn publishes(&self) -> &[String];
    /// Signal names this unit consumes (behaviors: empty).
    fn consumes(&self) -> &[String];
    /// Capabilities the unit requests (declaration only; never enforced here).
    fn caps(&self) -> Capabilities;

    /// Host setup at bind. Default no-op.
    fn bind(&mut self, _host: &mut dyn HostApi) {}
    /// Begin: resolve ids / warm up.
    fn start(&mut self, host: &mut dyn HostApi);
    /// One step of actual work.
    fn run(&mut self, host: &mut dyn HostApi, timing: Timing) -> TickOutcome;
    /// Release.
    fn stop(&mut self, host: &mut dyn HostApi);
}

// ---- BehaviorExecutionUnit: drives a real BehaviorNode through the HostApi ----

/// Wraps a `BehaviorNode` and bridges it to the [`HostApi`]. Owns a private
/// single-node `SignalStore` purely as the publish buffer the node writes into;
/// after `update` it mirrors each published slot onto the host. Touches nothing
/// in the live scheduler/store.
pub struct BehaviorExecutionUnit {
    node: Box<dyn BehaviorNode>,
    publishes: Vec<String>,
    consumes: Vec<String>,
    schema: Arc<SignalSchema>,
    publisher: crate::signal::SignalPublisher,
    reader: SignalReader,
    snap: SignalSnapshot,
    specs: BTreeMap<String, ParamSpec>,
    values: ParamMap,
}

impl BehaviorExecutionUnit {
    /// Build from a `BehaviorInit` (the product of a `BehaviorFactory`). The node
    /// moves *into the unit* — never into the runner (F1).
    pub fn from_init(init: BehaviorInit) -> Self {
        let mut builder = SignalSchemaBuilder::new();
        builder
            .publish_all(&init.publish)
            .expect("a behavior's own publish set is internally unique");
        let (schema, _warnings) = builder.finish();
        let (publisher, reader) = SignalStore::new(&schema);
        let snap = reader.snapshot();
        let publishes = init.publish.iter().map(|s| s.name.clone()).collect();
        Self {
            node: init.node,
            publishes,
            consumes: Vec::new(),
            schema,
            publisher,
            reader,
            snap,
            specs: init.specs,
            values: init.values,
        }
    }
}

impl ExecutionUnit for BehaviorExecutionUnit {
    fn publishes(&self) -> &[String] {
        &self.publishes
    }
    fn consumes(&self) -> &[String] {
        &self.consumes
    }
    fn caps(&self) -> Capabilities {
        Capabilities::default()
    }

    fn start(&mut self, _host: &mut dyn HostApi) {
        let config = ResolvedConfig::new(&self.specs, &self.values);
        let mut ctx = BehaviorStartCtx::new(&self.schema, config);
        self.node.start(&mut ctx);
    }

    fn run(&mut self, host: &mut dyn HostApi, timing: Timing) -> TickOutcome {
        // Split borrows so the node, its publisher, and the buffers can be touched
        // together (the node owns none of them).
        let Self {
            node,
            publisher,
            reader,
            snap,
            specs,
            values,
            schema,
            ..
        } = self;

        {
            let config = ResolvedConfig::new(specs, values);
            let mut ctx = BehaviorCtx::new("mock_instance".into(), None, publisher, config, timing);
            node.update(&mut ctx); // the proof: real BehaviorNode.update() runs
        }
        publisher.publish();
        reader.snapshot_into(snap);

        // Mirror each published slot onto the host — Factory→…→HostApi.publish().
        for (id, _name, _kind) in schema.iter() {
            host.publish(id, snap.get(id));
        }
        TickOutcome::Ok
    }

    fn stop(&mut self, _host: &mut dyn HostApi) {
        self.node.stop();
    }
}

// ---- MockExecutionUnit: a controllable fake for wiring/supervisor tests ----

/// A fake unit that records its lifecycle, exercises the host surface, and can be
/// told to fault. No real behavior; proves the runner/supervisor/host plumbing.
pub struct MockExecutionUnit {
    publishes: Vec<String>,
    caps: Capabilities,
    pub started: bool,
    pub stopped: bool,
    pub runs: u32,
    /// If set, `run` returns `Faulted` once `runs` exceeds it.
    fault_after: Option<u32>,
}

impl MockExecutionUnit {
    pub fn new(publishes: Vec<String>) -> Self {
        Self {
            publishes,
            caps: Capabilities::default(),
            started: false,
            stopped: false,
            runs: 0,
            fault_after: None,
        }
    }
    pub fn faulting_after(mut self, runs: u32) -> Self {
        self.fault_after = Some(runs);
        self
    }
    pub fn with_caps(mut self, caps: Capabilities) -> Self {
        self.caps = caps;
        self
    }
}

impl ExecutionUnit for MockExecutionUnit {
    fn publishes(&self) -> &[String] {
        &self.publishes
    }
    fn consumes(&self) -> &[String] {
        &[]
    }
    fn caps(&self) -> Capabilities {
        self.caps
    }

    fn start(&mut self, _host: &mut dyn HostApi) {
        self.started = true;
    }

    fn run(&mut self, host: &mut dyn HostApi, _timing: Timing) -> TickOutcome {
        self.runs += 1;

        // Prove the host surface is consumable from a unit.
        host.heartbeat();
        let _ = host.timing();
        let _ = host.metrics();
        let _ = host.request_frame(FrameTier::R320x180);
        if let Some(name) = self.publishes.first() {
            if let Some(id) = host.subscribe(name) {
                host.publish(id, SignalValue::F32(self.runs as f32));
            }
        }

        match self.fault_after {
            Some(n) if self.runs > n => TickOutcome::Faulted("mock fault".into()),
            _ => TickOutcome::Ok,
        }
    }

    fn stop(&mut self, _host: &mut dyn HostApi) {
        self.stopped = true;
    }
}
