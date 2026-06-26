//! Behavior Runtime — producer addons run here, off the render thread.
//!
//! A single dedicated thread owns a [`scheduler`] that drives every
//! [`BehaviorNode`]. Behaviors read the latest CPU frame and publish signals to
//! the shared [`SignalStore`](crate::signal::SignalStore); they never touch the
//! GPU and never block rendering. The render thread controls the runtime only
//! through a [`BehaviorHandle`] (non-blocking command channel + a stop flag).
//!
//! Producers reach the scheduler two ways, both ending in a `BehaviorInit`:
//! [`builtins`] (compiled-in reference producers) and [`addons`] (external
//! packages bound through the [`host`] seam — [`BehaviorRegistry`] +
//! [`BehaviorFactory`]). The scheduler does not know or care which path an
//! instance came from.

pub mod addons;
pub mod builtins;
pub mod host;
pub mod native;
pub mod node;
mod scheduler;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::addon::schema::{ParamMap, ParamSpec, ParamValue};
use crate::camera::FrameSource;
use crate::signal::{SignalPublisher, SignalSchema, SignalSpec};

pub use host::{BehaviorFactory, BehaviorHost, BehaviorRegistry};
pub use node::BehaviorNode;

use scheduler::BehaviorScheduler;

/// A constructed-but-not-yet-running behavior: the node plus its config. This
/// is the unit the engine builds and the [`BehaviorCommand::Reload`] payload.
pub struct BehaviorInit {
    pub instance_id: String,
    pub node: Box<dyn BehaviorNode>,
    /// Signals this behavior publishes (feeds the schema builder).
    pub publish: Vec<SignalSpec>,
    /// Manifest param specs (supply defaults for `ResolvedConfig`).
    pub specs: BTreeMap<String, ParamSpec>,
    /// Current config values; `SetParam` mutates these in place.
    pub values: ParamMap,
    pub enabled: bool,
}

/// Why an addon was skipped during instantiation (R3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    FilesystemMissing,
    ManualUninstall,
    TrustRevoked,
    LoadFailed(String),
}

/// Control messages sent to the behavior thread. All are applied at the top of
/// a tick, never mid-update.
pub enum BehaviorCommand {
    /// Structural reload: the render thread rebuilt the schema + store, so the
    /// behavior thread takes the new producer endpoint, the new schema, and the
    /// new behavior set together (stop old, start new).
    Reload {
        publisher: SignalPublisher,
        schema: Arc<SignalSchema>,
        behaviors: Vec<BehaviorInit>,
        /// Optional sync barrier: if provided, the behavior thread signals this
        /// after the reload is applied.
        sync: Option<mpsc::SyncSender<()>>,
    },
    /// Hot config update — does not recreate the instance or its resources.
    SetParam {
        instance_id: String,
        key: String,
        value: ParamValue,
    },
    Enable(String),
    Disable(String),
    Shutdown,
}

/// Spawns the behavior thread. Construction-only; the live thread is owned
/// through the returned [`BehaviorHandle`].
pub struct BehaviorRuntime;

impl BehaviorRuntime {
    /// Start the behavior thread with an initial behavior set. `publisher`,
    /// `schema`, and `frame` are owned by the thread for its lifetime; only the
    /// behavior *set* changes (via [`BehaviorCommand::Reload`]).
    pub fn spawn(
        publisher: SignalPublisher,
        schema: Arc<SignalSchema>,
        frame: FrameSource,
        initial: Vec<BehaviorInit>,
        artifact_tx: Option<std::sync::mpsc::Sender<(String, crate::runtime::artifact::RenderArtifact)>>,
    ) -> BehaviorHandle {
        let running = Arc::new(AtomicBool::new(true));
        let stats = Arc::new(BehaviorStatsShared::default());
        let (tx, rx) = mpsc::channel::<BehaviorCommand>();

        let flag = running.clone();
        let stats_thread = stats.clone();
        let handle = thread::Builder::new()
            .name("behavior".into())
            .spawn(move || {
                let sched =
                    BehaviorScheduler::new(publisher, schema, frame, initial, stats_thread, artifact_tx);
                sched.run(rx, flag);
            })
            .expect("failed to spawn behavior thread");

        BehaviorHandle {
            tx,
            running,
            handle: Some(handle),
            stats,
        }
    }
}

/// The render-thread-side control handle. Every method is a non-blocking
/// channel send (the render thread never waits on the behavior thread).
/// Dropping the handle signals shutdown and joins.
pub struct BehaviorHandle {
    tx: Sender<BehaviorCommand>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    stats: Arc<BehaviorStatsShared>,
}

impl BehaviorHandle {
    /// Hand the behavior thread a new store endpoint, schema, and behavior set
    /// (a structural reload).
    ///
    /// If `sync` is true, this waits up to 100ms for the behavior thread to
    /// acknowledge and apply the reload.
    pub fn reload(
        &self,
        publisher: SignalPublisher,
        schema: Arc<SignalSchema>,
        behaviors: Vec<BehaviorInit>,
        sync: bool,
    ) -> std::result::Result<(), String> {
        let (tx, rx) = if sync {
            let (tx, rx) = mpsc::sync_channel(1);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        self.tx
            .send(BehaviorCommand::Reload {
                publisher,
                schema,
                behaviors,
                sync: tx,
            })
            .map_err(|e| format!("failed to send reload: {e}"))?;

        if let Some(rx) = rx {
            // Wait for the behavior thread to apply the reload. 100ms is enough
            // for ~3 behavior ticks; if it takes longer, the worker is likely
            // stalled (R2).
            if rx.recv_timeout(Duration::from_millis(100)).is_err() {
                return Err("behavior reload timeout (worker stalled)".to_string());
            }
        }

        Ok(())
    }

    pub fn set_param(&self, instance_id: &str, key: &str, value: ParamValue) {
        let _ = self.tx.send(BehaviorCommand::SetParam {
            instance_id: instance_id.to_string(),
            key: key.to_string(),
            value,
        });
    }

    pub fn set_enabled(&self, instance_id: &str, enabled: bool) {
        let cmd = if enabled {
            BehaviorCommand::Enable(instance_id.to_string())
        } else {
            BehaviorCommand::Disable(instance_id.to_string())
        };
        let _ = self.tx.send(cmd);
    }

    /// A snapshot of the live behavior metrics.
    pub fn stats(&self) -> BehaviorStats {
        self.stats.snapshot()
    }
}

impl Drop for BehaviorHandle {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = self.tx.send(BehaviorCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Live behavior metrics, snapshotted for the inspector / logs.
#[derive(Clone, Copy, Debug, Default)]
pub struct BehaviorStats {
    /// Behavior-thread ticks per second (target ~30).
    pub fps: f32,
    /// Wall time of the last tick's `update` pass, in microseconds.
    pub last_update_us: f32,
    /// Total publishes since start.
    #[allow(dead_code)] // surfaced to tests/inspector; not in the render-loop log
    pub published: u64,
    /// Number of ticks whose update pass exceeded the 8ms budget.
    #[allow(dead_code)] // surfaced to tests/inspector; not in the render-loop log
    pub over_budget: u64,
}

/// Atomic backing for [`BehaviorStats`]. Writes are a few relaxed stores per
/// tick on the behavior thread (negligible); the budget *warning* print is
/// debug-only, so release builds carry no instrumentation I/O.
#[derive(Default)]
pub(crate) struct BehaviorStatsShared {
    fps_bits: AtomicU32,
    last_update_ns: AtomicU64,
    published: AtomicU64,
    over_budget: AtomicU64,
}

impl BehaviorStatsShared {
    pub(crate) fn record_tick(&self, update: Duration, published: u64, over_budget: bool) {
        self.last_update_ns
            .store(update.as_nanos() as u64, Ordering::Relaxed);
        self.published.store(published, Ordering::Relaxed);
        if over_budget {
            self.over_budget.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn set_fps(&self, fps: f32) {
        self.fps_bits.store(fps.to_bits(), Ordering::Relaxed);
    }

    fn snapshot(&self) -> BehaviorStats {
        BehaviorStats {
            fps: f32::from_bits(self.fps_bits.load(Ordering::Relaxed)),
            last_update_us: self.last_update_ns.load(Ordering::Relaxed) as f32 / 1000.0,
            published: self.published.load(Ordering::Relaxed),
            over_budget: self.over_budget.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::{SignalKind, SignalSchema, SignalStore};

    /// End-to-end thread lifecycle: spawn → real publishing → drop joins
    /// cleanly (no hang). The deterministic per-tick behavior is covered by the
    /// scheduler unit tests.
    #[test]
    fn spawn_publishes_then_shuts_down_cleanly() {
        let schema = Arc::new(SignalSchema::from_pairs(&[("signal.time", SignalKind::F32)]));
        let (publisher, reader) = SignalStore::new(&schema);
        let handle = BehaviorRuntime::spawn(
            publisher,
            schema,
            FrameSource::empty(),
            vec![builtins::time::init_with("time".into(), Default::default(), true)],
            None,
        );

        // Bounded wait for the 30Hz thread to publish at least once.
        let mut waited = 0;
        while reader.published() == 0 && waited < 200 {
            thread::sleep(Duration::from_millis(5));
            waited += 1;
        }
        assert!(reader.published() > 0, "behavior thread must publish");
        assert!(handle.stats().published > 0);

        // Dropping the handle signals shutdown and joins; the test simply
        // returning proves the join did not hang.
        drop(handle);
    }
}
