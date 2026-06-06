// ascii_mask.wgsl — a signal-driven ASCII face mask overlay.
//
// Composed with the engine prelude (common.wgsl): @group(0) host context
// (H.resolution, H.time), @group(1) frame input (sample_rgb), and the
// fullscreen vertex stage. This addon adds @group(2) params and reads @group(3)
// signals — the same contract the builtin CRT uses. Animation comes entirely
// from @group(3) (refreshed per frame via prepare → write_buffer); there is no
// rebuild, no bind-group recreation, no text system, and no font.

// @group(2): params packed by the runner in SORTED-KEY order as f32s, padded to
// 16 bytes: [ascii_expression, mask_size, opacity]. `ascii_expression` is a text
// param → packs as 0.0 (single-expression mode; the glyph is procedural).
struct Params {
    ascii_expression: f32,
    mask_size: f32,
    opacity: f32,
    _pad: f32,
};
@group(2) @binding(0) var<uniform> P: Params;

// @group(3): one vec4<f32> per consumed signal, in manifest `consume` order.
//   v[0].xy = face.position [-1,+1]   v[1].x = face.rotation (rad)   v[2].x = face.scale [0,1]
// Optional + unpublished → 0.0 fallback, so with no behavior the overlay hides.
struct Signals {
    v: array<vec4<f32>, 3>,
};
@group(3) @binding(0) var<uniform> S: Signals;

// Distance from point `p` to segment `a`–`b` (for procedural glyph strokes).
fn seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let img = sample_rgb(in.uv);

    let pos = S.v[0].xy;
    let rot = S.v[1].x;
    let scl = S.v[2].x;

    // Presence: scale ~0 (no face / fallback) → hidden; smooth fade-in as the
    // tracked face grows. This is the "no signal → hide" rule, done as a fade.
    let presence = smoothstep(0.02, 0.18, scl);
    let alpha = clamp(P.opacity, 0.0, 1.0) * presence;
    if (alpha <= 0.001) {
        return vec4<f32>(img, 1.0);
    }

    // Face centre in uv space. X is mirrored to match the selfie preview
    // (dot-renderer mirror=true is the default): raw-right → screen-left.
    // Y: +1 = top.
    let center = vec2<f32>(0.5 - pos.x * 0.5, 0.5 - pos.y * 0.5);

    // Glyph half-size in screen-height units; signal scale → 0.5..2.0 × mask_size.
    let half = max(P.mask_size, 0.001) * mix(0.5, 2.0, clamp(scl, 0.0, 1.0));

    // Fragment → glyph-local space: translate, aspect-correct, rotate, scale.
    let aspect = H.resolution.x / max(H.resolution.y, 1.0);
    var d = in.uv - center;
    d.x = d.x * aspect;
    let c = cos(rot);
    let sn = sin(rot);
    let r = vec2<f32>(c * d.x - sn * d.y, sn * d.x + c * d.y);
    let local = r / half; // glyph occupies roughly [-1,1]^2

    // Cheap reject outside the glyph box → passthrough.
    if (abs(local.x) > 1.3 || abs(local.y) > 1.3) {
        return vec4<f32>(img, 1.0);
    }

    // ">_<": left eye '>', mouth '_', right eye '<'. Pure line segments.
    let stroke = 0.10;
    var dmin = 1e9;
    // left eye '>'
    dmin = min(dmin, seg(local, vec2<f32>(-0.70, 0.45), vec2<f32>(-0.32, 0.12)));
    dmin = min(dmin, seg(local, vec2<f32>(-0.32, 0.12), vec2<f32>(-0.70, -0.18)));
    // right eye '<'
    dmin = min(dmin, seg(local, vec2<f32>(0.70, 0.45), vec2<f32>(0.32, 0.12)));
    dmin = min(dmin, seg(local, vec2<f32>(0.32, 0.12), vec2<f32>(0.70, -0.18)));
    // mouth '_'
    dmin = min(dmin, seg(local, vec2<f32>(-0.34, -0.55), vec2<f32>(0.34, -0.55)));

    let ink = 1.0 - smoothstep(stroke - 0.04, stroke + 0.04, dmin);

    // Surveillance tracking box: a thin frame around the glyph.
    let box_d = abs(max(abs(local.x), abs(local.y)) - 1.15);
    let frame = (1.0 - smoothstep(0.0, 0.03, box_d)) * 0.35;

    // CRT scanlines + faint flicker on the overlay marks (retro terminal feel).
    let scan = 0.8 + 0.2 * sin(in.uv.y * H.resolution.y * 1.2);
    let flicker = 0.9 + 0.1 * sin(H.time * 30.0);

    let phosphor = vec3<f32>(0.55, 1.0, 0.65); // terminal green
    let mark = max(ink, frame) * scan * flicker;

    let outc = mix(img, phosphor, clamp(mark * alpha, 0.0, 1.0));
    return vec4<f32>(outc, 1.0);
}
