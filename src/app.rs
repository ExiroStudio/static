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

use crate::camera::{CameraId, CaptureMode, WebcamCapture};
use crate::engine::{Engine, load_pipeline_config};
use crate::ui::Ui;

#[derive(Default)]
pub struct App {
    window: Option<Arc<Window>>,
    engine: Option<Engine>,
    camera: Option<WebcamCapture>,
    ui: Option<Ui>,
    fullscreen: bool,
    current_device_id: Option<CameraId>,
    current_mode: Option<CaptureMode>,
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
                        .with_inner_size(LogicalSize::new(960.0, 720.0))
                        .with_resizable(false),
                )
                .expect("failed to create window"),
        );

        let mut config = load_pipeline_config();
        let device_id = config.source.config.get("device_id")
            .and_then(|v| serde_json::from_value::<CameraId>(v.clone()).ok())
            .unwrap_or(CameraId::Index(0));

        let saved_mode = {
            let conf = &config.source.config;
            let w = conf.get("width").and_then(|v| v.as_u64());
            let h = conf.get("height").and_then(|v| v.as_u64());
            let f = conf.get("fps").and_then(|v| v.as_u64());
            let fmt = conf.get("pixel_format").and_then(|v| v.as_str());
            
            if let (Some(w), Some(h), Some(f), Some(fmt)) = (w, h, f, fmt) {
                Some(CaptureMode {
                    width: w as u32,
                    height: h as u32,
                    fps: f as u32,
                    pixel_format: fmt.to_string(),
                })
            } else {
                None
            }
        };

        let modes = crate::camera::available_modes(&device_id);
        let active_mode = if let Some(saved) = &saved_mode {
            if modes.contains(saved) {
                Some(saved.clone())
            } else {
                crate::camera::select_best_mode(&modes)
            }
        } else {
            crate::camera::select_best_mode(&modes)
        };

        // Update config to reflect what we actually use so UI is consistent
        if let Some(ref m) = active_mode {
            if !config.source.config.is_object() {
                config.source.config = serde_json::Value::Object(Default::default());
            }
            if let Some(obj) = config.source.config.as_object_mut() {
                obj.insert("width".to_string(), serde_json::json!(m.width));
                obj.insert("height".to_string(), serde_json::json!(m.height));
                obj.insert("fps".to_string(), serde_json::json!(m.fps));
                obj.insert("pixel_format".to_string(), serde_json::json!(m.pixel_format));
            }
        }

        let camera = WebcamCapture::new(device_id.clone(), active_mode.clone()).expect("failed to open webcam");
        let engine = pollster::block_on(Engine::new(
            window.clone(),
            camera.width,
            camera.height,
            camera.frame_source(),
            config,
        ));
        let mut ui = Ui::new(&window, engine.device(), engine.surface_format());
        ui.state.available_cameras = crate::camera::available_cameras();
        ui.state.available_modes = modes;
        ui.state.camera_status = crate::camera::CameraStatus::Starting;

        self.window = Some(window);
        self.camera = Some(camera);
        self.engine = Some(engine);
        self.ui = Some(ui);
        self.current_device_id = Some(device_id);
        self.current_mode = active_mode;
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

        let new_device_id = engine.config().source.config.get("device_id")
            .and_then(|v| serde_json::from_value::<CameraId>(v.clone()).ok())
            .unwrap_or(CameraId::Index(0));
            
        let new_mode = {
            let conf = &engine.config().source.config;
            let w = conf.get("width").and_then(|v| v.as_u64());
            let h = conf.get("height").and_then(|v| v.as_u64());
            let f = conf.get("fps").and_then(|v| v.as_u64());
            let fmt = conf.get("pixel_format").and_then(|v| v.as_str());
            
            if let (Some(w), Some(h), Some(f), Some(fmt)) = (w, h, f, fmt) {
                Some(CaptureMode {
                    width: w as u32,
                    height: h as u32,
                    fps: f as u32,
                    pixel_format: fmt.to_string(),
                })
            } else {
                None
            }
        };
        
        if Some(&new_device_id) != self.current_device_id.as_ref() {
            let modes = crate::camera::available_modes(&new_device_id);
            ui.state.available_modes = modes.clone();
            
            let mut active_mode = new_mode.clone();
            if let Some(m) = &active_mode {
                if !modes.contains(m) {
                    active_mode = None;
                }
            }
            if active_mode.is_none() {
                active_mode = crate::camera::select_best_mode(&modes);
                engine.set_camera(new_device_id.clone(), active_mode.clone());
            }

            self.current_device_id = Some(new_device_id.clone());
            self.current_mode = active_mode.clone();
            camera.set_device(new_device_id, active_mode);
        } else if new_mode != self.current_mode {
            self.current_mode = new_mode.clone();
            camera.set_device(new_device_id, new_mode);
        }

        ui.state.camera_status = camera.status.lock().unwrap().clone();

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
