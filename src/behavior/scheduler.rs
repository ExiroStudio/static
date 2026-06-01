//! The behavior thread's scheduler: a single, deterministic loop that drains
//! commands, pulls the latest frame, runs the enabled behaviors in a stable
//! order, and commits their signals once per tick. No parallelism, no rayon,
//! and no channel traffic mid-tick (commands are drained only at the top).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::addon::schema::{ParamMap, ParamSpec};
use crate::camera::FrameSource;
use crate::runtime::ResolvedConfig;
use crate::signal::{SignalPublisher, SignalSchema};

use super::node::{BehaviorCtx, BehaviorStartCtx, FrameView, Timing};
use super::{BehaviorCommand, BehaviorInit, BehaviorStatsShared};

/// ~30 Hz tick period.
const TICK_PERIOD: Duration = Duration::from_millis(33);
/// Per-tick update budget; exceeding it bumps a stat (and warns in debug).
const UPDATE_BUDGET: Duration = Duration::from_millis(8);

/// One live behavior plus its owned, mutable config (so `SetParam` is a hot
/// update that never recreates the instance).
struct Slot {
    instance_id: String,
    node: Box<dyn super::node::BehaviorNode>,
    specs: BTreeMap<String, ParamSpec>,
    values: ParamMap,
    enabled: bool,
    started: bool,
}

impl Slot {
    fn from_init(init: BehaviorInit) -> Self {
        Slot {
            instance_id: init.instance_id,
            node: init.node,
            specs: init.specs,
            values: init.values,
            enabled: init.enabled,
            started: false,
        }
    }
}

pub(super) struct BehaviorScheduler {
    slots: Vec<Slot>,
    publisher: SignalPublisher,
    schema: SignalSchema,
    frame: FrameSource,
    frame_buf: Vec<u8>,
    stats: Arc<BehaviorStatsShared>,

    start: Instant,
    last_update: Instant,
    published: u64,
    fps_window_start: Instant,
    fps_window_ticks: u32,
}

impl BehaviorScheduler {
    pub(super) fn new(
        publisher: SignalPublisher,
        schema: SignalSchema,
        frame: FrameSource,
        initial: Vec<BehaviorInit>,
        stats: Arc<BehaviorStatsShared>,
    ) -> Self {
        let now = Instant::now();
        BehaviorScheduler {
            slots: initial.into_iter().map(Slot::from_init).collect(),
            publisher,
            schema,
            frame,
            frame_buf: Vec::new(),
            stats,
            start: now,
            last_update: now,
            published: 0,
            fps_window_start: now,
            fps_window_ticks: 0,
        }
    }

    /// The thread entry point: start behaviors, then loop until `running` clears.
    pub(super) fn run(mut self, rx: Receiver<BehaviorCommand>, running: Arc<AtomicBool>) {
        self.start_all();
        while running.load(Ordering::Relaxed) {
            let tick_start = Instant::now();

            // Drain all pending commands (non-blocking) before the tick.
            loop {
                match rx.try_recv() {
                    Ok(BehaviorCommand::Shutdown) => {
                        running.store(false, Ordering::Relaxed);
                        break;
                    }
                    Ok(cmd) => self.apply(cmd),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        running.store(false, Ordering::Relaxed);
                        break;
                    }
                }
            }
            if !running.load(Ordering::Relaxed) {
                break;
            }

            self.tick();

            let spent = tick_start.elapsed();
            if spent < TICK_PERIOD {
                thread::sleep(TICK_PERIOD - spent);
            }
        }
        self.stop_all();
    }

    fn start_all(&mut self) {
        let schema = self.schema;
        for slot in self.slots.iter_mut() {
            if !slot.started {
                let config = ResolvedConfig::new(&slot.specs, &slot.values);
                let mut ctx = BehaviorStartCtx::new(&schema, config);
                slot.node.start(&mut ctx);
                slot.started = true;
            }
        }
    }

    fn stop_all(&mut self) {
        for slot in self.slots.iter_mut() {
            if slot.started {
                slot.node.stop();
                slot.started = false;
            }
        }
    }

    /// One scheduler tick: frame → enabled behaviors → single atomic publish.
    fn tick(&mut self) {
        let dims = self.frame.peek(&mut self.frame_buf);

        let now = Instant::now();
        let dt = now.saturating_duration_since(self.last_update).as_secs_f32();
        self.last_update = now;
        let elapsed = self.start.elapsed().as_secs_f32();
        let timing = Timing { dt, elapsed };

        // Split borrows so the per-behavior loop can touch slots, the publisher,
        // and the frame buffer at once.
        let publisher = &mut self.publisher;
        let frame_buf = &self.frame_buf;
        let slots = &mut self.slots;

        let update_start = Instant::now();
        for slot in slots.iter_mut().filter(|s| s.enabled) {
            let frame = dims.map(|(w, h)| FrameView::new(w, h, frame_buf));
            let config = ResolvedConfig::new(&slot.specs, &slot.values);
            let mut ctx = BehaviorCtx::new(frame, &mut *publisher, config, timing);
            slot.node.update(&mut ctx);
        }
        let update_time = update_start.elapsed();

        // Commit every staged signal atomically — one buffer swap per tick.
        self.publisher.publish();
        self.published += 1;

        let over_budget = update_time > UPDATE_BUDGET;
        if over_budget {
            #[cfg(debug_assertions)]
            eprintln!(
                "[behavior] update budget exceeded: {:.2}ms > {}ms",
                update_time.as_secs_f32() * 1000.0,
                UPDATE_BUDGET.as_millis(),
            );
        }
        self.stats
            .record_tick(update_time, self.published, over_budget);

        // Recompute behavior FPS once per second.
        self.fps_window_ticks += 1;
        let window = self.fps_window_start.elapsed().as_secs_f32();
        if window >= 1.0 {
            self.stats.set_fps(self.fps_window_ticks as f32 / window);
            self.fps_window_ticks = 0;
            self.fps_window_start = Instant::now();
        }
    }

    fn apply(&mut self, cmd: BehaviorCommand) {
        match cmd {
            BehaviorCommand::Reload(inits) => {
                self.stop_all();
                self.slots = inits.into_iter().map(Slot::from_init).collect();
                self.start_all();
            }
            BehaviorCommand::SetParam {
                instance_id,
                key,
                value,
            } => {
                if let Some(slot) = self.slots.iter_mut().find(|s| s.instance_id == instance_id) {
                    slot.values.insert(key, value);
                }
            }
            BehaviorCommand::Enable(id) => self.set_enabled(&id, true),
            BehaviorCommand::Disable(id) => self.set_enabled(&id, false),
            // Shutdown is handled by the run loop (clears `running`).
            BehaviorCommand::Shutdown => {}
        }
    }

    fn set_enabled(&mut self, instance_id: &str, enabled: bool) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.instance_id == instance_id) {
            slot.enabled = enabled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::addon::schema::{ParamValue, UiHints};
    use crate::signal::{SignalId, SignalReader, SignalStore, SignalValue};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    // ---- a probe behavior that records lifecycle calls and publishes a
    // ---- config-driven value to `signal.time` so tests can observe it ----
    struct Probe {
        started: Arc<AtomicBool>,
        stopped: Arc<AtomicBool>,
        updates: Arc<AtomicU32>,
        slow: bool,
        time_id: Option<SignalId>,
    }

    impl super::super::node::BehaviorNode for Probe {
        fn start(&mut self, ctx: &mut BehaviorStartCtx) {
            self.time_id = ctx.schema().id("signal.time");
            self.started.store(true, Ordering::Relaxed);
        }
        fn update(&mut self, ctx: &mut BehaviorCtx) {
            self.updates.fetch_add(1, Ordering::Relaxed);
            if self.slow {
                thread::sleep(Duration::from_millis(10));
            }
            if let Some(id) = self.time_id {
                let v = ctx.config().f32("v");
                ctx.publish(id, SignalValue::F32(v));
            }
        }
        fn stop(&mut self) {
            self.stopped.store(true, Ordering::Relaxed);
        }
    }

    fn f32_spec(default: f32) -> ParamSpec {
        ParamSpec::F32 {
            default,
            min: Some(0.0),
            max: Some(10.0),
            ui: UiHints::default(),
        }
    }

    struct Handles {
        started: Arc<AtomicBool>,
        stopped: Arc<AtomicBool>,
        updates: Arc<AtomicU32>,
    }

    fn probe_init(id: &str, default_v: f32, slow: bool) -> (BehaviorInit, Handles) {
        let started = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let updates = Arc::new(AtomicU32::new(0));
        let node = Probe {
            started: started.clone(),
            stopped: stopped.clone(),
            updates: updates.clone(),
            slow,
            time_id: None,
        };
        let mut specs = BTreeMap::new();
        specs.insert("v".to_string(), f32_spec(default_v));
        let mut values = ParamMap::new();
        values.insert("v".to_string(), ParamValue::F32(default_v as f64));
        let init = BehaviorInit {
            instance_id: id.to_string(),
            node: Box::new(node),
            specs,
            values,
            enabled: true,
        };
        (init, Handles { started, stopped, updates })
    }

    fn make(initial: Vec<BehaviorInit>) -> (BehaviorScheduler, SignalReader) {
        let schema = SignalSchema::standard();
        let (publisher, reader) = SignalStore::new(&schema);
        let stats = Arc::new(BehaviorStatsShared::default());
        let sched = BehaviorScheduler::new(publisher, schema, FrameSource::empty(), initial, stats);
        (sched, reader)
    }

    fn read_time(reader: &mut SignalReader) -> f32 {
        let mut snap = reader.snapshot();
        reader.snapshot_into(&mut snap);
        let id = SignalSchema::standard().id("signal.time").unwrap();
        snap.get(id).as_f32().unwrap()
    }

    #[test]
    fn start_and_stop_are_invoked() {
        let (init, h) = probe_init("p", 1.0, false);
        let (mut sched, _r) = make(vec![init]);
        sched.start_all();
        assert!(h.started.load(Ordering::Relaxed), "start() must run");
        sched.stop_all();
        assert!(h.stopped.load(Ordering::Relaxed), "stop() must run");
    }

    #[test]
    fn update_publishes_signal() {
        let (init, _h) = probe_init("p", 1.0, false);
        let (mut sched, mut reader) = make(vec![init]);
        sched.start_all();
        sched.tick();
        assert_eq!(read_time(&mut reader), 1.0);
    }

    #[test]
    fn set_param_is_a_hot_update() {
        let (init, _h) = probe_init("p", 1.0, false);
        let (mut sched, mut reader) = make(vec![init]);
        sched.start_all();
        sched.apply(BehaviorCommand::SetParam {
            instance_id: "p".into(),
            key: "v".into(),
            value: ParamValue::F32(0.25),
        });
        sched.tick();
        assert_eq!(read_time(&mut reader), 0.25, "SetParam must change published value");
    }

    #[test]
    fn disable_skips_update_enable_resumes() {
        let (init, h) = probe_init("p", 1.0, false);
        let (mut sched, _r) = make(vec![init]);
        sched.start_all();
        sched.apply(BehaviorCommand::Disable("p".into()));
        sched.tick();
        assert_eq!(h.updates.load(Ordering::Relaxed), 0, "disabled behavior must not update");
        sched.apply(BehaviorCommand::Enable("p".into()));
        sched.tick();
        assert_eq!(h.updates.load(Ordering::Relaxed), 1, "re-enabled behavior must update");
    }

    #[test]
    fn reload_stops_old_and_starts_new() {
        let (init_a, a) = probe_init("a", 1.0, false);
        let (mut sched, mut reader) = make(vec![init_a]);
        sched.start_all();

        let (init_b, b) = probe_init("b", 2.0, false);
        sched.apply(BehaviorCommand::Reload(vec![init_b]));
        assert!(a.stopped.load(Ordering::Relaxed), "reload must stop the old set");
        assert!(b.started.load(Ordering::Relaxed), "reload must start the new set");

        sched.tick();
        assert_eq!(read_time(&mut reader), 2.0, "new behavior publishes after reload");
        assert_eq!(b.updates.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn over_budget_update_is_counted() {
        let (init, _h) = probe_init("slow", 1.0, true); // sleeps 10ms > 8ms budget
        let (mut sched, _r) = make(vec![init]);
        sched.start_all();
        sched.tick();
        assert!(
            sched.stats.snapshot().over_budget >= 1,
            "an update exceeding the 8ms budget must be counted"
        );
    }

    // `update()` cannot call `build()` or touch the GPU: BehaviorCtx exposes
    // only frame/publish/config/timing — there is no runtime/device/queue
    // handle to call. This is a compile-time guarantee, not a runtime check.
}
