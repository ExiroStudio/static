# Filters

A **filter** (a `pipeline`-kind addon) is a signal *consumer* and the unit of
the render chain. It receives the previous node's frame, records one or more GPU
passes, and writes the next frame. It knows nothing of its neighbours, position,
or the source/sink.

## The contract

```rust
pub trait FilterNode {
    fn prepare(&mut self, queue: &Queue, signals: &SignalSnapshot) {} // upload-only; default no-op
    fn process(&self, ctx: &mut FrameContext);                        // record-only
}
```

* `prepare` runs once per frame *before* recording: fold the latest snapshot
  into the node's per-frame uniforms via `queue.write_buffer`. It must **only
  update** existing resources — never recreate bind groups/pipelines, never
  rebuild. Filters that consume nothing keep the default no-op.
* `process` records the render pass into `ctx.output`, binding
  `[host, input, params, (signals)]`.

`FrameContext` hands a node exactly what it needs: the encoder, `@group(0)` host
bind group, `@group(1)` input bind group, and the output `TextureView`.

## Two ways a filter exists

| | Builtin | External |
|--|---------|----------|
| Code | Implements `BuiltinAddon` (own factory) | None — data only |
| Shipped as | Inside the binary | A ZIP: manifest + WGSL + assets |
| Params → `@group(2)` | Typed `#[repr(C)]` struct | f32 array, sorted-key order |
| Signals → `@group(3)` | Same `SignalsBinding` | Same `SignalsBinding` |
| Executor sees | `Box<dyn FilterNode>` | `Box<dyn FilterNode>` |

Both run through the identical executor. The builtin **CRT** is the reference
signal-consuming filter; the [`filter_signal_consume`](../examples/filter_signal_consume)
and [`external_shader_signal`](../examples/external_shader_signal) examples are
the reference *external* consumers.

## The generic external-shader runner

An external pipeline addon ships no compiled code. When the runtime resolves a
node whose addon has no compiled factory, it falls back to the runner:

```
read manifest.shaders[fragment]  →  compose with prelude (common.wgsl)
pack numeric params (sorted-key) →  @group(2) uniform, padded to 16 bytes
if manifest.consume != []        →  build @group(3) SignalsBinding (Task 1)
                                     prepare() refreshes it every frame
```

The result is a node identical in shape to the builtin CRT: it reads the same
`@group(3)` packing and refreshes per frame with no rebuild. An addon that
declares no `consume` gets no `@group(3)`, so its layout is unchanged.

## Instantiation

```rust
fn instantiate(device, host_layout, image_layout, format,
               config: &ResolvedConfig,      // manifest defaults filled in
               signals: &SignalContext)       // resolve declared consume → SignalId, once
    -> Box<dyn FilterNode>;
```

`ResolvedConfig` gives typed accessors (`f32`, `bool`, …) that always fall back
to the declared default — an addon never has to handle a missing or wrong-typed
param (validation already ran). `SignalContext` resolves only what the addon
*declared* in `consume`; asking for an undeclared signal returns `None`.

## Rules

* Resolve signal ids **once** at instantiate; address by id thereafter.
* Per frame, only `prepare`'s `write_buffer` — never a rebuild. This is what
  keeps `build_count` constant while signals animate.
* A node is size-independent: it reads resolution from the host uniform, so it
  survives a resize untouched.

See [GROUP3](GROUP3.md) for the consume/packing detail and
[ADDON_GUIDE](ADDON_GUIDE.md) to author one.
