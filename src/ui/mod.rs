//! UI v1 — the egui product overlay.
//!
//! A dark, OBS/VS-Code/Discord-flavoured workspace painted *on top of* the live
//! GPU preview. It shows the pipeline as a stack of addon cards you can toggle
//! and select, plus a properties panel generated entirely from each addon's
//! parameter schema. It hides every engine internal: the only things it touches
//! are [`Engine`]'s thin edit/read API and the [`PipelineConfig`] document.
//!
//! [`Engine`]: crate::engine::Engine
//! [`PipelineConfig`]: crate::addon::pipeline::PipelineConfig

mod input;
mod panels;
mod state;
mod widgets;

pub use state::UiState;

use egui::{ClippedPrimitive, Context, TexturesDelta};
use egui_wgpu::{Renderer, ScreenDescriptor};
use wgpu::*;
use winit::event::WindowEvent;
use winit::window::Window;

use crate::engine::Engine;
use input::EguiInput;

/// Tessellated UI for one frame, produced by [`Ui::build`] and consumed by
/// [`Ui::paint`]. Built while `Engine` is borrowed (the UI reads/mutates the
/// config); painted later when the GPU device is available — so the two never
/// borrow `Engine` at once.
struct PaintData {
    jobs: Vec<ClippedPrimitive>,
    textures_delta: TexturesDelta,
    pixels_per_point: f32,
}

/// egui glue: the context, the winit input/output plumbing, the wgpu renderer,
/// and the persistent UI state.
pub struct Ui {
    ctx: Context,
    input: EguiInput,
    renderer: Renderer,
    pub state: UiState,
    pending: Option<PaintData>,
}

impl Ui {
    pub fn new(window: &Window, device: &Device, surface_format: TextureFormat) -> Self {
        let ctx = Context::default();
        widgets::install_theme(&ctx);

        let size = window.inner_size();
        let input = EguiInput::new(
            [size.width.max(1), size.height.max(1)],
            window.scale_factor() as f32,
        );

        // Paint straight into the sRGB swapchain; no depth, single-sampled.
        let renderer = Renderer::new(device, surface_format, None, 1);

        Self {
            ctx,
            input,
            renderer,
            state: UiState::default(),
            pending: None,
        }
    }

    /// Feed a window event to egui first. Returns whether egui consumed it (so
    /// the app can skip its own shortcut handling for that event).
    pub fn on_window_event(&mut self, event: &WindowEvent) -> bool {
        let wants_pointer = self.ctx.wants_pointer_input();
        let wants_keyboard = self.ctx.wants_keyboard_input();
        self.input
            .on_window_event(event, wants_pointer, wants_keyboard)
    }

    /// Build the UI for this frame against `engine` (reading/mutating its
    /// config). Stashes the tessellated result for [`paint`](Self::paint). Must
    /// be called while no other `Engine` borrow is live.
    pub fn build(&mut self, engine: &mut Engine) {
        self.ctx.set_pixels_per_point(self.input.scale());
        let raw_input = self.input.take_raw_input();
        let ctx = self.ctx.clone();
        let full_output = ctx.run(raw_input, |ctx| {
            panels::draw(ctx, engine, &mut self.state);
        });
        // platform_output (clipboard/cursor/IME) is intentionally ignored in v1.

        let jobs = ctx.tessellate(full_output.shapes, full_output.pixels_per_point);
        self.pending = Some(PaintData {
            jobs,
            textures_delta: full_output.textures_delta,
            pixels_per_point: full_output.pixels_per_point,
        });
    }

    /// Paint the UI built by [`build`](Self::build) on top of `view` (the
    /// surface frame the engine just rendered the preview into), in its own
    /// encoder, loading the existing contents so it composites over the preview.
    pub fn paint(&mut self, device: &Device, queue: &Queue, view: &TextureView, size_px: [u32; 2]) {
        let Some(data) = self.pending.take() else {
            return;
        };
        let screen = ScreenDescriptor {
            size_in_pixels: size_px,
            pixels_per_point: data.pixels_per_point,
        };

        for (id, delta) in &data.textures_delta.set {
            self.renderer.update_texture(device, queue, *id, delta);
        }

        let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
            label: Some("egui_encoder"),
        });
        let user_cmds = self
            .renderer
            .update_buffers(device, queue, &mut encoder, &data.jobs, &screen);
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("egui_pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Load, // composite over the preview
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.renderer.render(&mut pass, &data.jobs, &screen);
        }
        // User-callback buffers (if any) must land before the egui draw.
        queue.submit(user_cmds.into_iter().chain(std::iter::once(encoder.finish())));

        for id in &data.textures_delta.free {
            self.renderer.free_texture(id);
        }
    }
}
