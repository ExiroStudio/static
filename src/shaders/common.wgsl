// common.wgsl
// Shared prelude prepended to every addon fragment shader by
// `effects::make_module`. WGSL has no `#include`, so we compose at
// pipeline-build time.
//
// Bind group contract every pipeline addon sees:
//
//   @group(0) host context   — resolution + time, owned by the runtime.
//   @group(1) frame input    — the texture produced by the previous node
//                              (or the source for the first node).
//   @group(2) addon params    — declared by each addon's own shader, filled
//                              from its manifest params. The prelude does NOT
//                              declare it; addons are independent.

// --- @group(0): host context, identical for every node -------------------
struct Host {
    resolution: vec2<f32>,
    time: f32,
    _pad: f32,
};
@group(0) @binding(0) var<uniform> H: Host;

// --- @group(1): the frame input (previous node's output / the source) ----
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

fn sample_rgb(uv: vec2<f32>) -> vec3<f32> {
    return textureSample(input_tex, input_sampler, uv).rgb;
}
