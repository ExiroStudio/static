//! BehaviorRuntime — behaviors run here, off the render thread.
//!
//! A single dedicated thread owns the behavior loop. It publishes signals to
//! the shared [`SignalBus`] at its own cadence and never touches the GPU or the
//! render thread, so a slow behavior can only age a signal — it can never stall
//! a frame.
//!
//! Spike scope: one hardcoded behavior that publishes `signal.time = sin(t)` at
//! ~60 Hz. The general behavior trait, frame access, and config wiring are the
//! full migration's job; this proves the thread + bus path only.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::signal::{SignalBus, SignalValue};

/// Owns the behavior thread. Dropping it signals the thread to stop and joins.
pub struct BehaviorRuntime {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl BehaviorRuntime {
    /// Spawn the behavior thread against a shared bus.
    pub fn spawn(bus: Arc<SignalBus>) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();

        let handle = thread::Builder::new()
            .name("behavior".into())
            .spawn(move || {
                let start = Instant::now();
                while flag.load(Ordering::Relaxed) {
                    let t = start.elapsed().as_secs_f32();
                    bus.publish("signal.time", SignalValue::F32(t.sin()));
                    thread::sleep(Duration::from_millis(16));
                }
            })
            .expect("failed to spawn behavior thread");

        Self {
            running,
            handle: Some(handle),
        }
    }
}

impl Drop for BehaviorRuntime {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
