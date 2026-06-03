# Static

A realtime **signal runtime** for live video. Point it at your webcam and it
renders the feed through a configurable chain of GPU effects — dot/ASCII
rasterizers, CRT scanlines, glitch passes — that you reorder, retune, and extend
live. The render chain is **not hardcoded**: it's described by `pipeline.json`
and executed as `source → filters → sink`. Every "look" is an *addon*; the engine
special-cases none of them.

```
  webcam ──▶ [ crt ] ──▶ [ dot-renderer ] ──▶ window
                              ▲
                              └─ signals (e.g. signal.time) drive animation per frame
```

## Why it's built this way

- **Modular by contract.** A builtin effect and a dragged-in third-party addon
  run through the *identical* execution path. There is no per-effect branching
  anywhere in the executor.
- **Two decoupled threads.** A GPU **render thread** (~144 fps) and a
  deterministic **behavior thread** (~30 Hz) share only a lock-free, triple-buffered
  signal store — no mutex on the hot path, no channels carrying pixels, no
  per-frame allocation.
- **Animation never rebuilds the pipeline.** Signals animate effects via
  `queue.write_buffer` into existing uniforms. The pipeline only rebuilds on a
  structural config edit — asserted by smoke tests and visible in the stats line.
- **Extensible without recompiling.** New looks ship as ZIP packages of *data*
  (a manifest + WGSL shader), installed by dragging onto the window.

## Quick start

Requires a Rust toolchain (`edition = "2024"`), a GPU/driver supported by
[`wgpu`](https://wgpu.rs), and a V4L2 webcam on Linux.

```bash
cargo run --release
```

On launch the engine reads [`pipeline.json`](pipeline.json), opens the webcam,
and renders to a window. The default pipeline mirrors the webcam through a
dot-renderer ("surveillance dots"); CRT is shipped but disabled. Toggle the
egui overlay to edit effects live.

```bash
cargo test          # includes offline WGSL validation (naga) + pipeline smoke tests
```

## The pipeline

`pipeline.json` is the single source of truth for what runs:

```json
{
  "format_version": 2,
  "source":  { "type": "webcam", "config": {} },
  "pipeline": [
    { "instance_id": "node-crt", "addon": "crt",          "enabled": false, "config": { … } },
    { "instance_id": "node-dot", "addon": "dot-renderer", "enabled": true,  "config": { … } }
  ],
  "behaviors": [
    { "instance_id": "beh-time", "addon": "time", "enabled": true, "config": {} }
  ],
  "sink": { "type": "window", "config": {} }
}
```

- **source** — where frames come in (webcam). Engine-shipped.
- **pipeline** — the ordered filter chain. Each entry references an addon by id.
- **behaviors** — signal producers running on the behavior thread.
- **sink** — where frames go out (window). Engine-shipped.

A UI edit mutates the in-memory config and, after a short debounce, atomically
rebuilds the chain. A rejected edit leaves the running pipeline untouched.

## Signals — how effects animate

A **signal** is a small fixed-size value (`f32`, `vec2`…`vec4`, `bool`, `i32`)
that a behavior *publishes* and a filter *consumes*. Signals are the only data
crossing the behavior→render boundary, copied one consistent frame at a time.

```
behaviors.publish ─┐
                   ├─▶ SignalSchema ─▶ @group(3) uniform (one vec4 per consumed signal)
filters.consume   ─┘
```

For example, the builtin `time` behavior publishes `signal.time = sin(elapsed)`,
and a filter that declares `consume = ["signal.time"]` reads it at `@group(3)` to
pulse or warp the image — with no pipeline rebuild as the value oscillates.

## Architecture at a glance

| Module | Responsibility |
|--------|----------------|
| `engine/` | Owns GPU + source + the editable `PipelineConfig`; orchestrates reload. |
| `runtime/` | Turns a `PipelineConfig` into a running filter chain; per-frame render. |
| `behavior/` | The behavior thread, deterministic ~30 Hz scheduler, `BehaviorNode` contract. |
| `signal/` | `SignalValue`/`SignalSchema`/`SignalStore` (the lock-free triple buffer). |
| `addon/` | Manifests, registry, `pipeline.json`, packaging, compat, errors. |
| `addons/` | Builtin addons (CRT, DotRenderer) + the generic external-shader runner. |
| `effects/` | Shader prelude composition + fullscreen pipeline helper. |
| `ui/` | The egui overlay (edits the config; never touches runtime internals). |

The full map lives in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). The engine
v2 contracts (architecture, signal/behavior/filter runtimes, `@group(3)`, config
format, manifest schema) are **frozen** — new functionality arrives as addons,
not engine edits ([`docs/FREEZE_REVIEW.md`](docs/FREEZE_REVIEW.md)).

## Writing an addon

An addon is a plain ZIP — a `manifest.toml` plus, for filters, a WGSL shader. No
Rust, no recompiling, no linking. Drag the ZIP onto the window to install.

```toml
manifest_version = 1
id      = "my-addon"
name    = "My Addon"
version = "1.0.0"
api_min = 1
api_max = 1
kind    = "pipeline"            # or "behavior"

[[shaders]]
id = "main"; path = "shaders/effect.wgsl"; stage = "fragment"; entry = "fs_main"

[params.intensity]
type = "f32"; default = 0.6; min = 0.0; max = 1.0; label = "Intensity"

[[consume]]
name = "signal.time"; kind = "f32"; optional = true
```

The worked references in [`examples/`](examples) are canonical — a behavior, a
signal-consuming filter, an external shader, and the `glitch-monitor` preset.
See [`docs/ADDON_GUIDE.md`](docs/ADDON_GUIDE.md) for the full author's guide.

## Documentation

| Doc | Covers |
|-----|--------|
| [ARCHITECTURE](docs/ARCHITECTURE.md) | The two threads, module map, core invariants. |
| [SIGNALS](docs/SIGNALS.md) | Signal types, schema, the publish↔consume contract. |
| [BEHAVIORS](docs/BEHAVIORS.md) | The behavior thread, scheduler, reload model. |
| [FILTERS](docs/FILTERS.md) | The `FilterNode` execution interface. |
| [GROUP3](docs/GROUP3.md) | The `@group(3)` signals uniform layout. |
| [CONFIG_V2](docs/CONFIG_V2.md) | The `pipeline.json` format. |
| [ADDON_GUIDE](docs/ADDON_GUIDE.md) | Building addons without touching the engine. |
| [FREEZE_REVIEW](docs/FREEZE_REVIEW.md) | What's frozen, extension points, stability tiers. |

## Tech stack

Rust · [`wgpu`](https://wgpu.rs) 0.20 (WGSL shaders) · [`winit`](https://github.com/rust-windowing/winit) 0.30 ·
[`egui`](https://github.com/emilk/egui) 0.28 (hand-rolled winit input bridge) ·
[`nokhwa`](https://github.com/l1npengtul/nokhwa) (webcam capture).
