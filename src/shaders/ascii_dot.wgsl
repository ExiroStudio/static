// ascii_dot.wgsl — the dot-renderer addon's look: luminance -> dot matrix.
//
// Per grid cell, sample the input luminance and draw a dot whose
// radius/brightness tracks it. Parameters arrive at @group(2), filled from the
// addon's manifest params — the shader never sees the source or the sink, only
// the frame input at @group(1).

struct DotParams {
    cell_size: f32,
    dot_softness: f32,
    contrast: f32,
    exposure: f32,
    glow: f32,
    mirror: f32,
    _pad0: f32,
    _pad1: f32,
};
@group(2) @binding(0) var<uniform> P: DotParams;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let res = H.resolution;
    let px = in.uv * res;

    // Snap to the dot grid; sample luminance at the cell centre.
    let cell = max(P.cell_size, 2.0);
    let cell_id = floor(px / cell);
    let cell_center = (cell_id + vec2<f32>(0.5)) * cell;

    var suv = cell_center / res;
    suv.x = mix(suv.x, 1.0 - suv.x, P.mirror); // selfie mirror

    let l = sample_luma(suv) * P.exposure;
    let intensity = pow(clamp(l, 0.0, 1.0), P.contrast);

    // The dot: radius/brightness track luminance.
    let local = (px - cell_center) / (cell * 0.5);
    let dist = length(local);
    let radius = sqrt(intensity);
    let dot_mask = 1.0 - smoothstep(radius - P.dot_softness, radius, dist);

    var v = dot_mask * intensity;
    // Luminous core.
    v = v + intensity * intensity * P.glow * (1.0 - smoothstep(0.0, 1.6, dist));
    v = clamp(v, 0.0, 1.0);
    return vec4<f32>(v, v, v, 1.0);
}
