//! DotRenderer — the engine's original built-in look, now a builtin *addon*.
//!
//! Luminance → dot matrix. It used to be hardcoded into the engine; it now
//! registers through the addon system and runs through the same
//! [`FilterNode`] interface as every other addon. The runtime does not know
//! it exists by name.

use wgpu::*;

use super::{base_manifest, bool_param, build_shader_node, f32_param};
use crate::addon::manifest::Manifest;
use crate::runtime::{BuiltinAddon, FilterNode, ResolvedConfig, SignalContext};

/// `@group(2)` params block — mirrors `DotParams` in `ascii_dot.wgsl`
/// (8 × f32 = 32 bytes, 16-byte aligned).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DotParams {
    cell_size: f32,
    dot_softness: f32,
    contrast: f32,
    exposure: f32,
    glow: f32,
    mirror: f32,
    _pad0: f32,
    _pad1: f32,
}

pub struct DotRendererAddon;

impl BuiltinAddon for DotRendererAddon {
    fn manifest() -> Manifest {
        let mut m = base_manifest(
            "dot-renderer",
            "Dot Renderer",
            "Luminance reconstructed as a matrix of dots.",
        );
        m.tags = vec!["renderer".into(), "halftone".into()];
        m.params.insert(
            "cell_size".into(),
            f32_param(6.0, 2.0, 32.0, "Cell Size"),
        );
        m.params
            .insert("dot_softness".into(), f32_param(0.35, 0.0, 1.0, "Softness"));
        m.params
            .insert("contrast".into(), f32_param(1.4, 0.1, 4.0, "Contrast"));
        m.params
            .insert("exposure".into(), f32_param(1.0, 0.0, 4.0, "Exposure"));
        m.params
            .insert("glow".into(), f32_param(0.5, 0.0, 2.0, "Glow"));
        m.params
            .insert("mirror".into(), bool_param(true, "Mirror (selfie)"));
        m
    }

    fn instantiate(
        device: &Device,
        host_layout: &BindGroupLayout,
        image_layout: &BindGroupLayout,
        format: TextureFormat,
        config: &ResolvedConfig,
        _signals: &SignalContext,
    ) -> Box<dyn FilterNode> {
        let params = DotParams {
            cell_size: config.f32("cell_size"),
            dot_softness: config.f32("dot_softness"),
            contrast: config.f32("contrast"),
            exposure: config.f32("exposure"),
            glow: config.f32("glow"),
            mirror: if config.bool("mirror") { 1.0 } else { 0.0 },
            _pad0: 0.0,
            _pad1: 0.0,
        };
        build_shader_node(
            device,
            host_layout,
            image_layout,
            format,
            "dot_renderer",
            include_str!("../shaders/ascii_dot.wgsl"),
            params,
        )
    }
}
