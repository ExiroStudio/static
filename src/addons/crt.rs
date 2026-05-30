//! CRT — the first real pipeline addon.
//!
//! A surveillance-monitor look: monochrome phosphor, scanlines, a cheap
//! persistence bleed and subtle barrel distortion, all in a single pass. It is
//! a proper addon — registered through the registry, instantiated from its
//! manifest params, executed through the shared [`PipelineNode`] interface.
//! There is no `if addon == "crt"` anywhere; the runtime treats it exactly like
//! the DotRenderer.

use wgpu::*;

use super::{base_manifest, build_shader_node, f32_param};
use crate::addon::manifest::Manifest;
use crate::runtime::{BuiltinAddon, PipelineNode, ResolvedConfig};

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
    ) -> Box<dyn PipelineNode> {
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
        build_shader_node(
            device,
            host_layout,
            image_layout,
            format,
            "crt",
            include_str!("../shaders/crt.wgsl"),
            params,
        )
    }
}
