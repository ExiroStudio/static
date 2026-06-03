# filter_signal_consume (example)

The canonical answer to *"how does a filter read a live signal?"* A single-pass
filter that consumes `signal.time` and pulses brightness with it — no compiled
code, no rebuild when the signal changes.

## The whole contract, in three places

1. **Manifest** declares what it consumes:
   ```toml
   [[consume]]
   name = "signal.time"
   kind = "f32"
   optional = true
   ```
2. **Engine** resolves that to a `@group(3)` signals uniform at build and packs
   the latest snapshot into it every frame (`prepare` → `write_buffer`).
3. **Shader** reads the slot:
   ```wgsl
   struct Signals { v: array<vec4<f32>, 1> };
   @group(3) @binding(0) var<uniform> S: Signals;
   // ... S.v[0].x is signal.time
   ```

## Packing rules (the only things to get right)

- Each consumed signal occupies one `vec4<f32>` slot, in **manifest `consume`
  order**. Scalars use `.x`; `vec2/3/4` fill `.xy / .xyz / .xyzw`.
- `optional = true` + nobody publishes it → the slot reads `0.0` (fallback). The
  filter still loads and runs. A required (`optional = false`) signal that no
  behavior publishes is a build error instead.
- A filter that declares **no** `consume` gets no `@group(3)` at all — its
  pipeline layout is byte-identical to a non-consuming addon.

## Run it

Install (drag the zipped folder onto the window), then build a pipeline of
`webcam → filter-signal-consume → window` with the builtin `time` behavior
enabled. Watch brightness breathe — and confirm `build_count` never increments
while it does. See [docs/GROUP3.md](../../docs/GROUP3.md) and
[docs/FILTERS.md](../../docs/FILTERS.md).
