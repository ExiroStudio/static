# `@group(3)` — the signals uniform

`@group(3)` is the optional **dynamic bindings** group. Groups 0/1/2 (host /
input / params) are unchanged. `@group(3)` is created **only** when a filter
declares `consume = [...]`; filters that consume nothing never get it, so their
pipeline layout is byte-identical to before.

Its first (and currently only) binding is the per-frame **signals uniform**.
The group is intentionally generic — future dynamic resources (an atlas, an
overlay texture, a history buffer) can be added as additional bindings without
changing this contract.

## Packing rules

* One consumed signal → one `vec4<f32>` slot (16 bytes, std140 alignment). No
  per-kind packing logic: every kind widens into a `vec4`.
* Slots are ordered by the filter's **declared `consume` order** (manifest
  order for external addons; `m.consume` vec order for builtins).
* Scalars use `.x`; `vec2/3/4` fill `.xy / .xyz / .xyzw`. (See the kind→slot
  table in [SIGNALS](SIGNALS.md).)
* An **optional** signal that no behavior publishes resolves to `None` and packs
  as a zero slot — the **fallback**. A **required** missing signal is a build
  error instead, so the filter never silently renders garbage.

## The layout (Rust)

```rust
// runtime/signals_group.rs
pub fn signals_layout(device) -> BindGroupLayout    // binding 0, uniform, fragment-visible
pub struct SignalsBinding { /* buffer + bind group + resolved ids + scratch */ }
SignalsBinding::new(device, &layout, &signal_ctx) -> Option<Self>   // None ⇒ consume == []
binding.update(queue, &snapshot)                                    // pack, then write_buffer
```

`update` walks the resolved ids in declared order, widens each value to a
`vec4`, and uploads with a single `write_buffer` into a reused scratch buffer —
no per-frame allocation, no rebuild.

## The shader side

A consuming shader declares a matching uniform at `@group(3) @binding(0)`:

```wgsl
// N = number of consumed signals (manifest `consume` length)
struct Signals { v: array<vec4<f32>, N> };
@group(3) @binding(0) var<uniform> S: Signals;

// slot i, in declared consume order:
//   f32   → S.v[i].x
//   vec2  → S.v[i].xy
//   vec3  → S.v[i].xyz
//   vec4  → S.v[i]
```

`common.wgsl` declares `@group(0)`/`@group(1)` for every shader but **not**
`@group(2)`/`@group(3)` — those are the addon's own, so addons stay independent.

## Per-frame flow

```
render frame:
  reader.snapshot_into(&mut snap)          // one consistent frame
  node.prepare(queue, &snap):
      binding.update(queue, &snap)         // pack vec4 slots → write_buffer  (NO rebuild)
  node.process(ctx):
      pass.set_bind_group(3, signals_bg)   // shader reads S.v[…]
```

Builtin (CRT) and external (`filter_signal_consume`, `external_shader_signal`)
consumers share the exact same `SignalsBinding` path — Task 1 of the freeze made
the external runner build `@group(3)` identically to a builtin. The
`group3_bytes` stat reports `Σ consume.len() × 16` across enabled filters.

## Diagnostics invariant

While signals animate, the only GPU traffic for `@group(3)` is `write_buffer`.
`build_count` stays constant; recompilation never happens. This is asserted by
the smoke tests and the `signals_group` unit test (which checks each kind packs
into its 16-byte slot).
