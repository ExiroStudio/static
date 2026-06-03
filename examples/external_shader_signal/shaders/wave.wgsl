// wave.wgsl — an external shader driven by a consumed signal.
//
// Demonstrates the full external-addon contract: multiple @group(2) params
// (sorted-key packed) plus a @group(3) signal that animates the effect with no
// rebuild and no shader recompilation. signal.time drives a horizontal wave.

// @group(2): params in sorted-key order [amount, softness], padded to 16 bytes.
struct Params {
    amount: f32,
    softness: f32,
    _p0: f32,
    _p1: f32,
};
@group(2) @binding(0) var<uniform> P: Params;

// @group(3): consumed signals. Slot 0 = signal.time (.x = sin(elapsed)).
// Optional + unpublished → 0.0 fallback, so the wave simply freezes.
struct Signals {
    v: array<vec4<f32>, 1>,
};
@group(3) @binding(0) var<uniform> S: Signals;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let t = S.v[0].x; // signal.time, or 0.0 fallback

    // Phase the wave by the live signal — animation comes entirely from
    // @group(3), never from a rebuild.
    var uv = in.uv;
    uv.x = uv.x + sin(uv.y * 20.0 + t * 6.2831853) * P.amount * 0.02;

    let l = sample_luma(uv);

    // Soft threshold toward white-on-black; `softness` widens the ramp.
    let edge = mix(step(0.5, l), smoothstep(0.0, P.softness + 0.001, l - 0.25), P.softness);
    let out = clamp(edge, 0.0, 1.0);
    return vec4<f32>(out, out, out, 1.0);
}
