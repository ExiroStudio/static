//! Hand-rolled winit-0.30 → egui input bridge.
//!
//! egui-winit would normally do this, but its 0.28 release pins winit 0.29
//! (incompatible with our winit 0.30). egui *core* has no winit dependency, so
//! we translate the handful of window events egui needs ourselves and assemble
//! an [`egui::RawInput`] each frame. Clipboard / IME / cursor-icon round-trips
//! are intentionally out of scope for UI v1.

use std::time::Instant;

use egui::{Event, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Vec2};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub struct EguiInput {
    events: Vec<Event>,
    pointer_pos: Option<Pos2>,
    modifiers: Modifiers,
    scale: f32,
    size_px: [u32; 2],
    focused: bool,
    start: Instant,
}

impl EguiInput {
    pub fn new(size_px: [u32; 2], scale: f32) -> Self {
        Self {
            events: Vec::new(),
            pointer_pos: None,
            modifiers: Modifiers::default(),
            scale: scale.max(0.1),
            size_px,
            focused: true,
            start: Instant::now(),
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Assemble the accumulated input for this frame (draining the event queue).
    pub fn take_raw_input(&mut self) -> egui::RawInput {
        let size_pts = Vec2::new(
            self.size_px[0] as f32 / self.scale,
            self.size_px[1] as f32 / self.scale,
        );
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size_pts)),
            time: Some(self.start.elapsed().as_secs_f64()),
            modifiers: self.modifiers,
            events: std::mem::take(&mut self.events),
            focused: self.focused,
            ..Default::default()
        }
    }

    /// Translate one window event into egui input. `wants_pointer` /
    /// `wants_keyboard` come from the previous frame's context and decide
    /// whether egui "consumes" the event (so the app skips its own shortcuts).
    pub fn on_window_event(
        &mut self,
        event: &WindowEvent,
        wants_pointer: bool,
        wants_keyboard: bool,
    ) -> bool {
        match event {
            WindowEvent::Resized(size) => {
                self.size_px = [size.width.max(1), size.height.max(1)];
                false
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.scale = (*scale_factor as f32).max(0.1);
                false
            }
            WindowEvent::Focused(f) => {
                self.focused = *f;
                false
            }
            WindowEvent::ModifiersChanged(m) => {
                self.modifiers = map_modifiers(m.state());
                false
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pos = Pos2::new(position.x as f32 / self.scale, position.y as f32 / self.scale);
                self.pointer_pos = Some(pos);
                self.events.push(Event::PointerMoved(pos));
                wants_pointer
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer_pos = None;
                self.events.push(Event::PointerGone);
                false
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let (Some(pos), Some(button)) = (self.pointer_pos, map_button(*button)) {
                    self.events.push(Event::PointerButton {
                        pos,
                        button,
                        pressed: *state == ElementState::Pressed,
                        modifiers: self.modifiers,
                    });
                }
                wants_pointer
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (unit, delta) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (MouseWheelUnit::Line, Vec2::new(*x, *y)),
                    MouseScrollDelta::PixelDelta(p) => (
                        MouseWheelUnit::Point,
                        Vec2::new(p.x as f32 / self.scale, p.y as f32 / self.scale),
                    ),
                };
                self.events.push(Event::MouseWheel {
                    unit,
                    delta,
                    modifiers: self.modifiers,
                });
                wants_pointer
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if pressed && !self.modifiers.ctrl && !self.modifiers.command {
                    if let Some(text) = &event.text {
                        let s: String = text.chars().filter(|c| !c.is_control()).collect();
                        if !s.is_empty() {
                            self.events.push(Event::Text(s));
                        }
                    }
                }
                if let Some(key) = map_key(&event.logical_key) {
                    self.events.push(Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: event.repeat,
                        modifiers: self.modifiers,
                    });
                }
                wants_keyboard
            }
            _ => false,
        }
    }
}

fn map_modifiers(state: ModifiersState) -> Modifiers {
    Modifiers {
        alt: state.alt_key(),
        ctrl: state.control_key(),
        shift: state.shift_key(),
        mac_cmd: false,
        command: state.control_key(),
    }
}

fn map_button(b: MouseButton) -> Option<PointerButton> {
    match b {
        MouseButton::Left => Some(PointerButton::Primary),
        MouseButton::Right => Some(PointerButton::Secondary),
        MouseButton::Middle => Some(PointerButton::Middle),
        MouseButton::Back => Some(PointerButton::Extra1),
        MouseButton::Forward => Some(PointerButton::Extra2),
        MouseButton::Other(_) => None,
    }
}

/// Map the navigation/editing keys egui needs for widget interaction. Plain
/// character input flows through `Event::Text`, so letters/digits are omitted.
fn map_key(key: &Key) -> Option<egui::Key> {
    use egui::Key as E;
    let Key::Named(named) = key else {
        return None;
    };
    Some(match named {
        NamedKey::ArrowLeft => E::ArrowLeft,
        NamedKey::ArrowRight => E::ArrowRight,
        NamedKey::ArrowUp => E::ArrowUp,
        NamedKey::ArrowDown => E::ArrowDown,
        NamedKey::Escape => E::Escape,
        NamedKey::Tab => E::Tab,
        NamedKey::Backspace => E::Backspace,
        NamedKey::Enter => E::Enter,
        NamedKey::Space => E::Space,
        NamedKey::Insert => E::Insert,
        NamedKey::Delete => E::Delete,
        NamedKey::Home => E::Home,
        NamedKey::End => E::End,
        NamedKey::PageUp => E::PageUp,
        NamedKey::PageDown => E::PageDown,
        _ => return None,
    })
}
