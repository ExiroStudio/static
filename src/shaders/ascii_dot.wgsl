// ascii_dot.wgsl — the engine's one built-in look: luminance -> dot matrix.
//
// Pure and static: per grid cell, sample the webcam luminance and draw a dot
// whose radius/brightness tracks it. No edge / flicker / corruption / temporal
// terms — those belong to future addons. Drawn straight to the swapchain.

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let res = G.resolution;
    let px = in.uv * res;

    // Snap to the dot grid; sample luminance at the cell centre.
    let cell = max(G.cell_size, 2.0);
    let cell_id = floor(px / cell);
    let cell_center = (cell_id + vec2<f32>(0.5)) * cell;

    var suv = cell_center / res;
    suv.x = mix(suv.x, 1.0 - suv.x, G.mirror); // selfie mirror

    let l = sample_luma(suv) * G.exposure;
    let intensity = pow(clamp(l, 0.0, 1.0), G.contrast);

    // The dot: radius/brightness track luminance.
    let local = (px - cell_center) / (cell * 0.5);
    let dist = length(local);
    let radius = sqrt(intensity);
    let dot_mask = 1.0 - smoothstep(radius - G.dot_softness, radius, dist);

    var v = dot_mask * intensity;
    // Luminous core.
    v = v + intensity * intensity * G.glow * (1.0 - smoothstep(0.0, 1.6, dist));
    v = clamp(v, 0.0, 1.0);
    return vec4<f32>(v, v, v, 1.0);
}
