// pulse.wgsl — the minimal signal-consuming filter.
//
// Composed with the engine prelude (common.wgsl): @group(0) host context,
// @group(1) frame input (sample_luma / input_tex). This addon adds @group(2)
// params and reads @group(3) signals — the same contract the builtin CRT uses.

// @group(2): params, packed by the runner in sorted-key order as f32s, padded
// to 16 bytes. One real param (`strength`) + three pad floats = one vec4.
struct Params {
    strength: f32,
    _p0: f32,
    _p1: f32,
    _p2: f32,
};
@group(2) @binding(0) var<uniform> P: Params;

// @group(3): one vec4<f32> per consumed signal, in manifest `consume` order.
// This addon consumes signal.time (slot 0); .x carries sin(elapsed) in [-1, 1].
// If signal.time is optional + unpublished, the slot is 0.0 (steady image).
struct Signals {
    v: array<vec4<f32>, 1>,
};
@group(3) @binding(0) var<uniform> S: Signals;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let l = sample_luma(in.uv);

    // signal.time in [-1, 1] → a gentle brightness pulse, scaled by `strength`.
    let pulse = 1.0 + P.strength * S.v[0].x * 0.5;
    let out = clamp(l * pulse, 0.0, 1.0);

    return vec4<f32>(out, out, out, 1.0);
}
