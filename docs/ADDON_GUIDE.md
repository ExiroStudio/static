# Addon Author's Guide

This is the freeze deliverable: **what you can build without modifying the
engine.** An addon is a package of *data* — a manifest plus (for filters) a WGSL
shader and assets. No engine recompilation, no Rust, no linking.

The worked examples in [`examples/`](../examples) are the canonical references;
read them alongside this guide.

## Addon kinds

| Kind | Runs on | Does | v1 loadable externally? |
|------|---------|------|--------------------------|
| `pipeline` (filter) | render thread | consumes signals, renders a frame | **Yes** — ship WGSL |
| `behavior` (producer) | behavior thread | publishes signals | Reference only (Phase 3) |

> Sources and sinks are engine-shipped, not addons.

## Package layout (a plain ZIP)

```
my-addon.zip
├── manifest.toml          required, at archive root
├── shaders/effect.wgsl    files referenced by [[shaders]]
├── assets/…               files referenced by [[assets]]
└── README.md              optional
```

Install by dragging the ZIP onto the window. Installation validates the manifest
and extracts into `addons/<id>/`; the registry picks it up on the next scan.
Uninstall removes the directory (and any pipeline nodes using it).

## Manifest essentials

```toml
manifest_version = 1
id          = "my-addon"          # lowercase a-z 0-9 . _ -
name        = "My Addon"
version     = "1.0.0"
author      = "You"
api_min     = 1
api_max     = 1
kind        = "pipeline"          # or "behavior"

[[shaders]]                       # filters: at least one fragment shader
id = "main"; path = "shaders/effect.wgsl"; stage = "fragment"; entry = "fs_main"

[params.intensity]                # generates the properties UI; sorted-key packed
type = "f32"; default = 0.6; min = 0.0; max = 1.0; label = "Intensity"

[[consume]]                       # filters: signals to read at @group(3)
name = "signal.time"; kind = "f32"; optional = true
```

`kind = "behavior"` uses `[[publish]]` instead of `[[shaders]]`/`[[consume]]`.

## The lifecycle

```
author ZIP → install (validate manifest, extract) → registry scan
   → referenced in pipeline.json (UI add) → build():
        validate config → instantiate node:
            builtin?  → addon's own factory
            external? → generic shader runner: load WGSL, pack @group(2),
                        build @group(3) if consume != []
   → per frame: prepare() (write_buffer signals) → process() (record pass)
   → uninstall: remove nodes using it, delete dir, rescan
```

A config edit re-runs `build()` (debounced); a rejected build keeps the live
pipeline. Signal changes do **not** rebuild.

## Writing a filter shader

Your fragment shader is composed with the prelude `common.wgsl`, which already
provides `@group(0)` (`H.resolution`, `H.time`), `@group(1)` (`input_tex`,
`sample_luma`, `sample_rgb`), and the fullscreen vertex stage. You declare:

```wgsl
// @group(2): params, in SORTED-KEY order, padded to a 16-byte multiple.
struct Params { intensity: f32, _p0: f32, _p1: f32, _p2: f32 };
@group(2) @binding(0) var<uniform> P: Params;

// @group(3): one vec4 per consumed signal, in DECLARED consume order.
struct Signals { v: array<vec4<f32>, 1> };
@group(3) @binding(0) var<uniform> S: Signals;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let l = sample_luma(in.uv);
    let out = clamp(l * (1.0 + P.intensity * S.v[0].x), 0.0, 1.0);
    return vec4<f32>(out, out, out, 1.0);
}
```

### The two contracts you must hand-match

1. **`@group(2)` order = sorted manifest param keys.** The runner packs numeric
   params alphabetically as f32 and pads to 16 bytes. Your struct fields must be
   in that order. Non-numeric params pack as `0.0`. This is the one thing the
   runner cannot check for you.
2. **`@group(3)` order = declared `consume` order.** See [GROUP3](GROUP3.md).

## Common pitfalls

* **Uniform alignment** — a `@group(2)` struct must be a multiple of 16 bytes;
  pad with `_pad: f32` fields. The buffer is always padded; the struct must
  match.
* **Required vs optional consume** — a required signal nobody publishes fails the
  build. Mark signals `optional = true` and design a sensible zero-fallback.
* **Don't expect to rebuild from a shader** — animation comes from signals via
  `@group(3)`, never from re-instantiating.

## Validate before shipping

* `manifest.toml` parses and validates (ids unique, params' defaults satisfy
  their own spec, api range sane).
* The shader compiles when composed with the prelude — the engine's tests do
  exactly this (`naga` front-end) for the builtins and every bundled example.

See [FILTERS](FILTERS.md), [GROUP3](GROUP3.md), [SIGNALS](SIGNALS.md),
[BEHAVIORS](BEHAVIORS.md), and [CONFIG_V2](CONFIG_V2.md) for the underlying
contracts.
