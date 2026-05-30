//! Application shell: owns the window, webcam capture, engine and UI overlay,
//! and drives the frame loop.
//!
//! Controls:
//!   Tab   show/hide the config workspace (NORMAL ↔ CONFIG mode)
//!   Esc   close the workspace (or quit if it is already closed)
//!   F11   toggle borderless fullscreen
//!
//! The preview is rendered every frame regardless of mode; the workspace is an
//! egui overlay painted on top of it, so toggling it never interrupts rendering.

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
use crate::ui::Ui;

#[derive(Default)]
pub struct App {
    window: Option<Arc<Window>>,
    engine: Option<Engine>,
    camera: Option<WebcamCapture>,
    ui: Option<Ui>,
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
        let ui = Ui::new(&window, engine.device(), engine.surface_format());

        self.window = Some(window);
        self.camera = Some(camera);
        self.engine = Some(engine);
        self.ui = Some(ui);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };

        // egui gets first crack at every event; if it consumed the event we do
        // not also act on it (so typing in a text field doesn't toggle the UI).
        let consumed = self
            .ui
            .as_mut()
            .map(|ui| ui.on_window_event(&event))
            .unwrap_or(false);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(engine) = self.engine.as_mut() {
                    engine.resize(size.width, size.height);
                }
            }

            // Drag a .zip addon package onto the window to install it.
            WindowEvent::DroppedFile(path) => {
                let is_zip = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
                if is_zip {
                    if let (Some(engine), Some(ui)) = (self.engine.as_mut(), self.ui.as_mut()) {
                        ui.state.notice = Some(match engine.install_addon(&path) {
                            Ok(msg) => (false, msg),
                            Err(msg) => (true, msg),
                        });
                        ui.state.open = true; // reveal the workspace so the result is visible
                    }
                }
            }

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if !consumed => match logical_key {
                Key::Named(NamedKey::Tab) => {
                    if let Some(ui) = self.ui.as_mut() {
                        ui.state.toggle();
                    }
                }
                Key::Named(NamedKey::Escape) => {
                    let open = self.ui.as_ref().is_some_and(|ui| ui.state.open);
                    if open {
                        if let Some(ui) = self.ui.as_mut() {
                            ui.state.open = false;
                        }
                    } else {
                        event_loop.exit();
                    }
                }
                Key::Named(NamedKey::F11) => {
                    self.fullscreen = !self.fullscreen;
                    window.set_fullscreen(self.fullscreen.then(|| Fullscreen::Borderless(None)));
                }
                _ => {}
            },

            WindowEvent::RedrawRequested => self.redraw(),

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl App {
    /// One frame: apply any settled config edits, build the UI against the
    /// engine, then render the preview and paint the UI overlay on top.
    fn redraw(&mut self) {
        let (Some(engine), Some(ui), Some(camera)) = (
            self.engine.as_mut(),
            self.ui.as_mut(),
            self.camera.as_ref(),
        ) else {
            return;
        };

        engine.tick_reload();
        ui.build(engine);

        // The "Install from ZIP…" button defers the native picker to here, so
        // the dialog never runs nested inside the egui pass.
        if std::mem::take(&mut ui.state.want_install_picker) {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Addon package", &["zip"])
                .pick_file()
            {
                ui.state.notice = Some(match engine.install_addon(&path) {
                    Ok(msg) => (false, msg),
                    Err(msg) => (true, msg),
                });
            }
        }

        engine.render_with_overlay(camera, |device, queue, view, size| {
            ui.paint(device, queue, view, size);
        });
    }
}
