//! Static — a minimal modular realtime signal runtime.
//!
//! Core pipeline: webcam → DotRenderer → present. Everything else (effects,
//! overlays, tracking, behavior) is intended to live as external modular addons
//! rather than being baked into the core.

mod addon;
mod app;
mod camera;
mod effects;
mod engine;

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
