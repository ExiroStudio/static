# Face Tracking Lite

The first **executable external behavior addon** — proof that a behavior package
runs through the Phase 3 `BehaviorRegistry` seam with no engine dispatch edit.

```
webcam frame ─▶ grayscale+downscale ─▶ adaptive threshold ─▶ largest blob
            ─▶ bbox + image moments ─▶ EMA ─▶ publish(face.position/rotation/scale)
```

## Signals published

| Signal          | Kind   | Meaning                                            |
|-----------------|--------|----------------------------------------------------|
| `face.position` | `vec2` | bbox centre, normalised `[-1,+1]` (`+x` right, `+y` up) |
| `face.rotation` | `f32`  | major-axis orientation, radians `[-π/2,π/2]` (0 if unstable) |
| `face.scale`    | `f32`  | normalised bbox area `[0,1]`; also the presence cue |

## Params

| Key         | Range        | Effect                                        |
|-------------|--------------|-----------------------------------------------|
| `threshold` | `0..1`       | foreground sensitivity vs. mean luma          |
| `smoothing` | `0.02..1`    | EMA factor (lower = smoother, slower)         |

## How it executes

The package ships this manifest (identity + UI param schema + published signals).
Its executable code is compiled into the engine and bound to the id
`face-tracking-lite` via `register_behavior_with(...)`. The engine resolves
`pipeline.json` behavior entries to instances by **registry lookup**, never by a
hardcoded `match`. v1 loads no scripting/native/wasm — a real out-of-process
backend behind the same `BehaviorFactory` signature is Phase 3b.

## Constraints

CPU only. No GPU, OpenCV, ML, ONNX, or external dependency. Runs at ~30 Hz with
an ≤8 ms tick budget and no allocation on the steady-state path (analysis buffers
are sized once and reused). It is a vertical-slice tracker, **not** an accuracy
benchmark.
