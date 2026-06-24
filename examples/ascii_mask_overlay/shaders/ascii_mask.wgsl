// ascii_mask.wgsl — a signal-driven ASCII face mask overlay.
//
// Composed with the engine prelude (common.wgsl): @group(0) host context
// (H.resolution, H.time), @group(1) frame input (sample_rgb), and the
// fullscreen vertex stage. This addon adds @group(2) params and reads @group(3)
// signals — the same contract the builtin CRT uses. Animation comes entirely
// from @group(3) (refreshed per frame via prepare → write_buffer); there is no
// rebuild, no bind-group recreation, no text system, and no font.

// @group(2): params packed by the runner in SORTED-KEY order as f32s, padded to
// 16 bytes.
struct Params {
    ascii_expression: f32,
    mask_size: f32,
    mirror_x: f32,
    mirror_y: f32,
    offset_x: f32,
    offset_y: f32,
    opacity: f32,
    rotation_offset: f32,
    scale_mul: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
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
    let rot = S.v[1].z; // Use roll for 2D mask rotation
    let scl = S.v[2].x;

    // lebih cepat muncul
    let presence = smoothstep(0.01, 0.04, scl);
    let alpha = clamp(P.opacity, 0.0, 1.0) * presence;

    if (alpha <= 0.001) {
        return vec4<f32>(img, 1.0);
    }

    //--------------------------------------
    // CALIBRATION & COORDINATE FIX
    //--------------------------------------

    // 1. Coordinate normalization
    // Tracker provides pos in [0.0, 1.0] range.
    var center = pos;

    // 2. Mirror
    if (P.mirror_x > 0.5) {
        center.x = 1.0 - center.x;
    }
    if (P.mirror_y > 0.5) {
        center.y = 1.0 - center.y;
    }

    // 3. Offset
    center.x += P.offset_x;
    center.y += P.offset_y;

    // 4. Rotation Offset
    // Invert roll so it rotates the correct direction
    let rot_final = -rot + P.rotation_offset;

    // 5. Scale Multiplier
    let scl_final = scl * P.scale_mul;

    //--------------------------------------
    // SIZE FIX
    //--------------------------------------

    // Linear scale multiplier. 
    // scl is inter-eye distance (normalized, ~0.05 - 0.15).
    // Multiply by ~15 to map 0.067 to ~1.0 multiplier.
    let half = max(P.mask_size, 0.001) * (scl_final * 15.0);

    //--------------------------------------
    // ROTATION FIX
    //--------------------------------------

    let aspect = H.resolution.x / max(H.resolution.y, 1.0);

    var d = in.uv - center;

    d.x *= aspect;

    // Use pure rotation directly since signal is now stabilized
    let safe_rot = rot_final;

    let c = cos(safe_rot);
    let sn = sin(safe_rot);

    let r = vec2<f32>(
        c*d.x - sn*d.y,
        sn*d.x + c*d.y
    );

    var local = r / half; // glyph occupies roughly [-1,1]^2

    // Fix Y orientation: UV y=0 is top, our shape math assumes +y is UP.
    local.y = -local.y;

    //--------------------------------------
    // CULL
    //--------------------------------------

    if (
        abs(local.x) > 1.45 ||
        abs(local.y) > 1.45
    ) {
        return vec4<f32>(img, 1.0);
    }

    //--------------------------------------
    // ASCII
    //--------------------------------------

    let stroke = 0.08;

    var dmin = 1e9;

    dmin=min(dmin,seg(local,
        vec2<f32>(-0.70,0.42),
        vec2<f32>(-0.30,0.10)
    ));

    dmin=min(dmin,seg(local,
        vec2<f32>(-0.30,0.10),
        vec2<f32>(-0.70,-0.18)
    ));

    dmin=min(dmin,seg(local,
        vec2<f32>(0.70,0.42),
        vec2<f32>(0.30,0.10)
    ));

    dmin=min(dmin,seg(local,
        vec2<f32>(0.30,0.10),
        vec2<f32>(0.70,-0.18)
    ));

    dmin=min(dmin,seg(local,
        vec2<f32>(-0.28,-0.52),
        vec2<f32>(0.28,-0.52)
    ));

    let ink =
        1.0
        - smoothstep(
            stroke-0.03,
            stroke+0.03,
            dmin
        );

    //--------------------------------------
    // BOX
    //--------------------------------------

    let box_d =
        abs(
            max(
                abs(local.x),
                abs(local.y)
            ) - 1.10
        );

    let frame =
        (
            1.0
            - smoothstep(
                0.0,
                0.02,
                box_d
            )
        ) * 0.22;

    //--------------------------------------

    let scan =
        0.90
        + 0.10
        * sin(
            in.uv.y
            * H.resolution.y
        );

    let flicker =
        0.95
        + 0.05
        * sin(
            H.time*18.0
        );

    let phosphor =
        vec3<f32>(
            0.72,
            1.0,
            0.80
        );

    let mark =
        max(
            ink,
            frame
        )
        * scan
        * flicker;

    return vec4(
        mix(
            img,
            phosphor,
            mark*alpha
        ),
        1.0
    );
}