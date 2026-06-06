# FaceTrackingLite

The first **executable external behavior addon** (the producer half of the demo).
It proves the [Behavior Host](BEHAVIOR_HOST.md) seam end to end: an on-disk
package, bound to its id through the `BehaviorRegistry`, runs on the unchanged
behavior thread and publishes signals a filter consumes.

* Package: `examples/face_tracking_lite/` (canonical) → `addons/face_tracking_lite/` (install)
* Code: `src/behavior/addons/face_tracking_lite.rs` (registered via `register_behavior_with`)

## Algorithm (CPU only)

```
RGBA frame ─▶ grayscale + box-average downscale (width 80, aspect-matched)
          ─▶ adaptive threshold (mean × (0.6 + 0.9·threshold))
          ─▶ largest 4-connected foreground blob (iterative flood fill)
          ─▶ bbox + first/second image moments
          ─▶ position / rotation / scale
          ─▶ EMA smoothing (factor = `smoothing`)
          ─▶ publish (once per tick)
```

No GPU, OpenCV, ML, ONNX, or external dependency. Scratch buffers are sized once
(and only re-sized if the source resolution changes), so the steady-state tick
does **no** allocation. Target ≤8 ms at ~30 Hz; the analysis grid is ~80×N.

## Signals published

| Signal          | Kind   | Derivation                                        |
|-----------------|--------|---------------------------------------------------|
| `face.position` | `vec2` | bbox centre → `[-1,+1]` (`+x` right of raw image, `+y` up) |
| `face.rotation` | `f32`  | `½·atan2(2μ₁₁, μ₂₀−μ₀₂)`, negated to screen sense; **0** when near-circular (eccentricity < 0.15) |
| `face.scale`    | `f32`  | normalised bbox area `[0,1]`; doubles as the presence cue |

## Lost face

When no blob qualifies (area outside `[1%, 92%]` of the grid), the tracker
**smooth-decays** `scale`→0 and `rotation`→0 and holds the last position — it
never freezes and never spams. `scale`→0 makes the overlay fade out. Re-acquiring
snaps cleanly (the EMA seed resets once fully decayed).

## Params

| Key         | Default | Range     | Effect                                  |
|-------------|---------|-----------|-----------------------------------------|
| `threshold` | 0.5     | `0..1`    | foreground sensitivity vs. mean luma    |
| `smoothing` | 0.2     | `0.02..1` | EMA factor (lower = smoother, slower)   |

Edited live through the schema-driven Behavior panel; `SetParam` is a hot update
(no instance rebuild).

## Not in scope

No face mesh, landmarks, MediaPipe, expression/eye/mouth detection, multi-face,
or GPU tracking. This is a vertical-slice tracker, deliberately simple.
