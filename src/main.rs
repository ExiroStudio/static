//! Static — a minimal modular realtime signal runtime.
//!
//! The render chain is not hardcoded: it is loaded from `pipeline.json` and
//! executed by the [`runtime`] as `source → pipeline nodes → sink`. The
//! DotRenderer and CRT looks are builtin *addons* registered through the addon
//! ecosystem; the runtime executes them through one shared interface and
//! special-cases none of them. New looks arrive as addons, not engine edits.

mod addon;
mod addons;
mod app;
mod behavior;
mod camera;
mod effects;
mod engine;
pub mod native;
mod runner;
mod runtime;
mod signal;
mod ui;

#[cfg(test)]
mod smoke;

use winit::event_loop::{ControlFlow, EventLoop};

use crate::app::App;

fn main() {
    let event_loop = EventLoop::new().expect("failed to create event loop");

    // Continuous render loop: redraw every frame, not just on OS events.
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = App::default();
    event_loop
        .run_app(&mut app)
        .expect("event loop terminated with an error");
}
