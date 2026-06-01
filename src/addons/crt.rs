//! CRT — the first real pipeline addon.
//!
//! A surveillance-monitor look: monochrome phosphor, scanlines, a cheap
//! persistence bleed and subtle barrel distortion, all in a single pass. It is
//! a proper addon — registered through the registry, instantiated from its
//! manifest params, executed through the shared [`FilterNode`] interface.
//! There is no `if addon == "crt"` anywhere; the runtime treats it exactly like
//! the DotRenderer.
//!
//! CRT is also the spike's signal *consumer*: it reads `signal.time` from the
//! per-frame snapshot in [`prepare`](FilterNode::prepare) and modulates its
//! brightness uniform via `queue.write_buffer`. This proves the
//! behavior→bus→filter→GPU path with no rebuild, no new bind groups, and no
//! shader recompilation. (The declarative `consume = [...]` manifest form is
//! deferred to the full migration; a builtin knows its own signals in code,
//! just as it knows its own params.)

use wgpu::*;

use super::{base_manifest, build_shader_pass, f32_param, record_fullscreen_pass};
use crate::addon::manifest::Manifest;
use crate::runtime::{BuiltinAddon, FilterNode, FrameContext, ResolvedConfig};
use crate::signal::SignalSnapshot;

/// The signal CRT consumes, and how far it swings brightness around its
/// configured base. base 1.1 ± 0.8 → ~0.3..1.9: a clearly visible oscillation.
const TIME_SIGNAL: &str = "signal.time";
const BRIGHTNESS_AMPLITUDE: f32 = 0.8;

/// `@group(2)` params block — mirrors `CrtParams` in `crt.wgsl`
/// (8 × f32 = 32 bytes, 16-byte aligned).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CrtParams {
    scanline: f32,
    curvature: f32,
    persistence: f32,
    brightness: f32,
    vignette: f32,
    aperture: f32,
    _pad0: f32,
    _pad1: f32,
}

/// A live CRT node. Unlike the shared `ShaderNode`, it keeps its params buffer
/// and base values so it can rewrite the brightness field each frame from
/// `signal.time` — the same pipeline and bind group, only new bytes.
struct CrtNode {
    pipeline: RenderPipeline,
    params_bg: BindGroup,
    params_buf: Buffer,
    /// Current uniform contents; `brightness` is overwritten each frame.
    params: CrtParams,
    /// The configured brightness — the centre the signal oscillates around.
    base_brightness: f32,
}

impl FilterNode for CrtNode {
    fn prepare(&mut self, queue: &Queue, signals: &SignalSnapshot) {
        let t = signals.get(TIME_SIGNAL).unwrap_or(0.0);
        self.params.brightness = self.base_brightness + t * BRIGHTNESS_AMPLITUDE;
        // Update the existing uniform in place — no rebuild, no new resources.
        queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&self.params));
    }

    fn process(&self, ctx: &mut FrameContext) {
        record_fullscreen_pass(ctx, &self.pipeline, &self.params_bg, "crt");
    }
}

pub struct CrtAddon;

impl BuiltinAddon for CrtAddon {
    fn manifest() -> Manifest {
        let mut m = base_manifest(
            "crt",
            "CRT Monitor",
            "Monochrome CRT: scanlines, phosphor persistence, barrel distortion.",
        );
        m.tags = vec!["crt".into(), "retro".into(), "monitor".into()];
        m.params
            .insert("scanline".into(), f32_param(0.5, 0.0, 1.0, "Scanlines"));
        m.params
            .insert("curvature".into(), f32_param(0.4, 0.0, 1.0, "Curvature"));
        m.params.insert(
            "persistence".into(),
            f32_param(0.4, 0.0, 1.0, "Persistence"),
        );
        m.params
            .insert("brightness".into(), f32_param(1.1, 0.0, 3.0, "Brightness"));
        m.params
            .insert("vignette".into(), f32_param(0.5, 0.0, 1.0, "Vignette"));
        m.params
            .insert("aperture".into(), f32_param(0.5, 0.0, 1.0, "Aperture"));
        m
    }

    fn instantiate(
        device: &Device,
        host_layout: &BindGroupLayout,
        image_layout: &BindGroupLayout,
        format: TextureFormat,
        config: &ResolvedConfig,
    ) -> Box<dyn FilterNode> {
        let params = CrtParams {
            scanline: config.f32("scanline"),
            curvature: config.f32("curvature"),
            persistence: config.f32("persistence"),
            brightness: config.f32("brightness"),
            vignette: config.f32("vignette"),
            aperture: config.f32("aperture"),
            _pad0: 0.0,
            _pad1: 0.0,
        };
        let pass = build_shader_pass(
            device,
            host_layout,
            image_layout,
            format,
            "crt",
            include_str!("../shaders/crt.wgsl"),
            bytemuck::bytes_of(&params),
        );
        Box::new(CrtNode {
            pipeline: pass.pipeline,
            params_bg: pass.params_bg,
            params_buf: pass.params_buf,
            base_brightness: params.brightness,
            params,
        })
    }
}
