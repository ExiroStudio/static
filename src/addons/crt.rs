//! CRT — the first real pipeline addon, and the demo's signal *consumer*.
//!
//! A surveillance-monitor look in a single pass. It declares `consume`d signals
//! in its manifest; the runtime resolves them to a [`SignalContext`] at build,
//! and the node packs the latest snapshot into its `@group(3)` uniform each
//! frame (`prepare` → `write_buffer`). The shader reads `signal.time` from
//! `@group(3)` and modulates brightness — no rebuild, no new bind groups.
//!
//! `@group(2)` params (including the *base* brightness) are now written once at
//! build; only `@group(3)` changes per frame.

use wgpu::*;

use super::{base_manifest, build_shader_pass, f32_param, record_fullscreen_pass};
use crate::addon::manifest::Manifest;
use crate::runtime::{
    signals_layout, BuiltinAddon, FilterNode, FrameContext, ResolvedConfig, SignalContext,
    SignalsBinding,
};
use crate::signal::{SignalKind, SignalRef, SignalSnapshot};

/// The signal CRT consumes.
const TIME_SIGNAL: &str = "signal.time";

/// `@group(2)` params block — mirrors `CrtParams` in `crt.wgsl`
/// (8 × f32 = 32 bytes, 16-byte aligned). Written once at build.
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

/// A live CRT node. Holds its `@group(3)` signals binding and refreshes it each
/// frame; the static params bind group is built once and never rewritten.
struct CrtNode {
    pipeline: RenderPipeline,
    params_bg: BindGroup,
    _params_buf: Buffer,
    signals: SignalsBinding,
}

impl FilterNode for CrtNode {
    fn prepare(&mut self, queue: &Queue, signals: &SignalSnapshot) {
        // Pack the consumed signals into @group(3). No rebuild, no new resources.
        self.signals.update(queue, signals);
    }

    fn process(&self, ctx: &mut FrameContext) {
        record_fullscreen_pass(
            ctx,
            &self.pipeline,
            &self.params_bg,
            Some(self.signals.bind_group()),
            "crt",
        );
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
        // Consume signal.time (optional: degrades to a steady image if no
        // behavior publishes it).
        m.consume = vec![SignalRef {
            name: TIME_SIGNAL.into(),
            kind: SignalKind::F32,
            optional: true,
        }];
        m
    }

    fn instantiate(
        device: &Device,
        host_layout: &BindGroupLayout,
        image_layout: &BindGroupLayout,
        format: TextureFormat,
        config: &ResolvedConfig,
        signals: &SignalContext,
    ) -> Box<dyn FilterNode> {
        let params = CrtParams {
            scanline: config.f32("scanline"),
            curvature: config.f32("curvature"),
            persistence: config.f32("persistence"),
            brightness: config.f32("brightness"), // base; signal modulates in-shader
            vignette: config.f32("vignette"),
            aperture: config.f32("aperture"),
            _pad0: 0.0,
            _pad1: 0.0,
        };
        let slayout = signals_layout(device);
        let pass = build_shader_pass(
            device,
            host_layout,
            image_layout,
            format,
            "crt",
            include_str!("../shaders/crt.wgsl"),
            bytemuck::bytes_of(&params),
            &[&slayout],
        );
        let binding = SignalsBinding::new(device, &slayout, signals)
            .expect("crt declares a consumed signal, so it always has a group(3)");
        Box::new(CrtNode {
            pipeline: pass.pipeline,
            params_bg: pass.params_bg,
            _params_buf: pass.params_buf,
            signals: binding,
        })
    }
}
