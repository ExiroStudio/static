// glitch.wgsl — Glitch Monitor: monochrome surveillance corruption, single pass.
//
// Composed with the engine's shared prelude (common.wgsl), so it sees the host
// context at @group(0) (H.resolution, H.time), the previous node's frame at
// @group(1) (sample_luma / input_tex), and declares its own parameters at
// @group(2). White-on-black only; no HDR, no bloom, no temporal history.
//
// The engine's generic external-shader runner packs this addon's numeric
// parameters into @group(2) as a tight f32 array in sorted-key order:
// block_size, displacement, frequency, intensity (4 × f32 = 16 bytes, the
// uniform alignment unit). This struct must match that layout exactly.

struct GlitchParams {
    block_size: f32,
    displacement: f32,
    frequency: f32,
    intensity: f32,
};
@group(2) @binding(0) var<uniform> P: GlitchParams;

fn hash21(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let res = H.resolution;
    let t = H.time;
    var uv = in.uv;

    let block = max(P.block_size, 2.0);

    // Time quantised into discrete "disruption ticks"; higher frequency → faster.
    let tick = floor(t * mix(2.0, 30.0, P.frequency));

    // --- horizontal tearing: shift whole rows that are "active" this tick ---
    let row = floor(uv.y * res.y / block);
    let row_rng = hash21(vec2<f32>(row, tick));
    let row_active = step(1.0 - P.frequency * 0.6, row_rng);
    let tear = (hash21(vec2<f32>(row, tick + 1.0)) - 0.5) * 2.0 * P.displacement * row_active;
    uv.x = fract(uv.x + tear);

    // --- base monochrome signal ---
    var l = sample_luma(uv);

    // --- signal corruption: per-pixel-ish noise riding on the luminance ---
    let noise = hash21(vec2<f32>(floor(uv.x * res.x), row) + t) - 0.5;
    l = l + noise * P.intensity * 0.4;

    // --- random full-row scan disruption: invert the row's signal ---
    let scan_rng = hash21(vec2<f32>(row, tick * 3.1));
    let disrupt = step(1.0 - P.frequency * 0.15, scan_rng) * row_active;
    l = mix(l, 1.0 - l, disrupt);

    // --- block dropout: a few blocks snap to pure black or white ---
    let cell = floor(vec2<f32>(uv.x * res.x, uv.y * res.y) / block);
    let cell_rng = hash21(cell + tick * 1.7);
    if (cell_rng > 1.0 - P.intensity * 0.25) {
        l = step(0.5, hash21(cell + 9.3));
    }

    l = clamp(l, 0.0, 1.0);
    return vec4<f32>(l, l, l, 1.0);
}
