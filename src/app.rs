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

    settings_window: Option<Arc<Window>>,
    settings_surface: Option<wgpu::Surface<'static>>,
    settings_config: Option<wgpu::SurfaceConfiguration>,
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

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let is_main = self.window.as_ref().map_or(false, |w| w.id() == id);
        let is_settings = self.settings_window.as_ref().map_or(false, |w| w.id() == id);

        if !is_main && !is_settings {
            return;
        }

        let mut consumed = false;
        if is_settings {
            if let Some(ui) = self.ui.as_mut() {
                consumed = ui.on_window_event(&event);
            }
        }

        match event {
            WindowEvent::CloseRequested => {
                if is_main {
                    event_loop.exit();
                } else if is_settings {
                    if let Some(ui) = self.ui.as_mut() {
                        ui.state.open = false;
                    }
                    self.sync_settings_window(event_loop);
                }
            }
            WindowEvent::Resized(size) => {
                if is_main {
                    if let Some(engine) = self.engine.as_mut() {
                        engine.resize(size.width, size.height);
                    }
                } else if is_settings {
                    if let Some(config) = self.settings_config.as_mut() {
                        config.width = size.width.max(1);
                        config.height = size.height.max(1);
                        if let (Some(surface), Some(engine)) = (&self.settings_surface, &self.engine) {
                            surface.configure(engine.device(), config);
                        }
                    }
                }
            }
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
                        ui.state.open = true;
                        self.sync_settings_window(event_loop);
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
                        self.sync_settings_window(event_loop);
                    }
                }
                Key::Named(NamedKey::Escape) => {
                    let open = self.ui.as_ref().is_some_and(|ui| ui.state.open);
                    if open {
                        if let Some(ui) = self.ui.as_mut() {
                            ui.state.open = false;
                        }
                        self.sync_settings_window(event_loop);
                    } else if is_main {
                        event_loop.exit();
                    }
                }
                Key::Named(NamedKey::F11) => {
                    if is_main {
                        self.fullscreen = !self.fullscreen;
                        if let Some(window) = &self.window {
                            window.set_fullscreen(self.fullscreen.then(|| Fullscreen::Borderless(None)));
                        }
                    }
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
        let open = self.ui.as_ref().is_some_and(|ui| ui.state.open);
        if open {
            if let Some(settings) = &self.settings_window {
                settings.request_redraw();
            }
        }
    }
}

impl App {
    fn sync_settings_window(&mut self, event_loop: &ActiveEventLoop) {
        let open = self.ui.as_ref().is_some_and(|ui| ui.state.open);
        if self.settings_window.is_none() {
            if open {
                let window = Arc::new(
                    event_loop
                        .create_window(
                            Window::default_attributes()
                                .with_title("Settings")
                                .with_inner_size(LogicalSize::new(400.0, 720.0))
                                .with_resizable(true),
                        )
                        .expect("failed to create settings window"),
                );
                
                if let Some(engine) = self.engine.as_ref() {
                    let surface = engine.create_surface(window.clone());
                    let config = wgpu::SurfaceConfiguration {
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        format: engine.surface_format(),
                        width: window.inner_size().width.max(1),
                        height: window.inner_size().height.max(1),
                        present_mode: wgpu::PresentMode::Fifo,
                        alpha_mode: wgpu::CompositeAlphaMode::Auto,
                        view_formats: vec![],
                        desired_maximum_frame_latency: 2,
                    };
                    surface.configure(engine.device(), &config);
                    
                    self.settings_window = Some(window);
                    self.settings_surface = Some(surface);
                    self.settings_config = Some(config);
                }
            }
        } else {
            if let Some(window) = &self.settings_window {
                window.set_visible(open);
            }
        }
    }

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

        let open = ui.state.open;
        if open {
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
        }

        // Render preview (without UI overlay)
        engine.render_with_overlay(camera, |_device, _queue, _view, _size| {
            // No-op overlay since UI is in a separate window
        });

        // Render UI to the settings window if it exists and is open
        if open {
            if let (Some(surface), Some(config)) = (&self.settings_surface, &self.settings_config) {
                match surface.get_current_texture() {
                    Ok(frame) => {
                        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
                        let clear_color = wgpu::Color { r: 0.08, g: 0.08, b: 0.08, a: 1.0 };
                        ui.paint(engine.device(), engine.queue(), &view, [config.width, config.height], Some(clear_color));
                        frame.present();
                    }
                    Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                        surface.configure(engine.device(), config);
                    }
                    Err(e) => {
                        crate::trace_log!("WARNING", "Failed to get settings surface texture: {:?}", e);
                    }
                }
            }
        }
    }
}
