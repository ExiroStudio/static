// crt.wgsl — the CRT addon: surveillance-monitor look in a single pass.
//
// Monochrome phosphor, scanlines, a cheap persistence bleed and subtle barrel
// distortion. Deliberately one pass — no multi-pass bloom, no HDR, no temporal
// history. It reads only the frame input at @group(1); it has no idea what
// produced that frame or where it goes next.

struct CrtParams {
    scanline: f32,     // scanline darkness            0..1
    curvature: f32,    // barrel distortion amount     0..1
    persistence: f32,  // phosphor bleed into neighbours 0..1
    brightness: f32,   // output gain
    vignette: f32,     // edge darkening               0..1
    aperture: f32,     // scanline frequency scale     0..1
    _pad0: f32,
    _pad1: f32,
};
@group(2) @binding(0) var<uniform> P: CrtParams;

// Barrel-distort UV around the screen centre.
fn barrel(uv: vec2<f32>, amt: f32) -> vec2<f32> {
    let c = uv - vec2<f32>(0.5);
    let r2 = dot(c, c);
    return uv + c * r2 * amt;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let res = H.resolution;

    // Subtle tube curvature.
    let uv = barrel(in.uv, P.curvature * 0.35);

    // Past the glass edge → black bezel.
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Monochrome base luminance.
    var l = sample_luma(uv);

    // Phosphor persistence feel: a cheap 4-tap neighbour bleed (single pass,
    // no history texture). Lets bright cells smear into their neighbours.
    let texel = 1.0 / res;
    let bleed = (sample_luma(uv + vec2<f32>(texel.x, 0.0))
               + sample_luma(uv - vec2<f32>(texel.x, 0.0))
               + sample_luma(uv + vec2<f32>(0.0, texel.y))
               + sample_luma(uv - vec2<f32>(0.0, texel.y))) * 0.25;
    l = max(l, bleed * P.persistence);

    // Scanlines: darken alternate rows, with a slow vertical drift.
    let line_freq = res.y * mix(0.5, 1.0, P.aperture);
    let scan = sin((uv.y * line_freq + H.time * 2.0) * 3.14159265);
    let scan_mask = 1.0 - P.scanline * 0.5 * (scan * 0.5 + 0.5);
    l = l * scan_mask;

    // Vignette toward the corners.
    let c = uv - vec2<f32>(0.5);
    let vig = 1.0 - P.vignette * dot(c, c) * 2.0;
    l = l * clamp(vig, 0.0, 1.0);

    // Faint phosphor-green tint — near-monochrome surveillance monitor.
    l = clamp(l * P.brightness, 0.0, 1.0);
    let tint = vec3<f32>(0.85, 1.0, 0.87);
    return vec4<f32>(l * tint, 1.0);
}
