//! Application shell: owns the window, webcam capture and engine, and drives
//! the frame loop.
//!
//! Controls:
//!   Esc   quit
//!   F11   toggle borderless fullscreen

use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::{Key, NamedKey},
    window::{Fullscreen, Window, WindowId},
};

use crate::camera::WebcamCapture;
use crate::engine::Engine;

#[derive(Default)]
pub struct App {
    window: Option<Arc<Window>>,
    engine: Option<Engine>,
    camera: Option<WebcamCapture>,
    fullscreen: bool,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // initialise once
        }

        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("Static")
                        .with_inner_size(LogicalSize::new(800.0, 600.0)),
                )
                .expect("failed to create window"),
        );

        let camera = WebcamCapture::new().expect("failed to open webcam");
        let engine = pollster::block_on(Engine::new(window.clone(), camera.width, camera.height));

        self.window = Some(window);
        self.camera = Some(camera);
        self.engine = Some(engine);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => engine.resize(size.width, size.height),

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match logical_key {
                Key::Named(NamedKey::Escape) => event_loop.exit(),
                Key::Named(NamedKey::F11) => {
                    self.fullscreen = !self.fullscreen;
                    if let Some(window) = &self.window {
                        window.set_fullscreen(self.fullscreen.then(|| Fullscreen::Borderless(None)));
                    }
                }
                _ => {}
            },

            WindowEvent::RedrawRequested => {
                if let Some(camera) = &self.camera {
                    engine.render(camera);
                }
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}
