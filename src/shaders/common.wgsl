// common.wgsl
// Prepended to every fragment shader by `effects::make_module`. Declares the
// shared bind group interface, the fullscreen-triangle vertex stage, and small
// helpers. WGSL has no `#include`, so we compose at pipeline-build time.

// --- @group(0): the engine's small uniform block -------------------------
struct Globals {
    resolution: vec2<f32>,
    cell_size: f32,
    dot_softness: f32,
    contrast: f32,
    exposure: f32,
    glow: f32,
    mirror: f32,
};
@group(0) @binding(0) var<uniform> G: Globals;

// --- @group(1): the image input (the webcam) -----------------------------
@group(1) @binding(0) var input_tex: texture_2d<f32>;
@group(1) @binding(1) var input_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Single oversized triangle covering the screen (no vertex buffer).
@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let uv = vec2<f32>(f32((vid << 1u) & 2u), f32(vid & 2u));
    var out: VsOut;
    out.uv = uv;
    out.clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    return out;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.299, 0.587, 0.114));
}

fn sample_luma(uv: vec2<f32>) -> f32 {
    return luma(textureSample(input_tex, input_sampler, uv).rgb);
}
