# Engine v2 — Architecture

Static is a realtime signal runtime. The render chain is **not** hardcoded: it
is described by `pipeline.json` and executed as `source → filters → sink`. Every
"look" is an *addon*; the engine special-cases none of them.

This document is the map. The per-subsystem docs go deeper:
[SIGNALS](SIGNALS.md) · [BEHAVIORS](BEHAVIORS.md) · [FILTERS](FILTERS.md) ·
[GROUP3](GROUP3.md) · [CONFIG_V2](CONFIG_V2.md) · [ADDON_GUIDE](ADDON_GUIDE.md).

## The two threads

```
            ┌──────────────────────── render thread (≈144 fps) ───────────────────────┐
  webcam ─▶ │  source tex ─▶ filter ─▶ filter ─▶ … ─▶ sink (window)                     │
            │                  ▲ prepare() folds latest signal snapshot into @group(3)  │
            └──────────────────│──────────────────────────────────────────────────────┘
                               │  SignalSnapshot (one consistent frame, lock-free)
            ┌──────────────────│──────────────────────── behavior thread (≈30 Hz) ──────┐
            │  behavior ─▶ behavior ─▶ …  publish() one atomic frame of signals          │
            └───────────────────────────────────────────────────────────────────────────┘
```

* **Render thread** owns the GPU, the source texture, the live `PipelineConfig`,
  and the filter chain. It reads one `SignalSnapshot` per frame and renders.
* **Behavior thread** owns a deterministic ~30 Hz scheduler. Behaviors read the
  latest CPU frame and publish signals. They cannot touch the GPU.
* The two share **only** the lock-free [`SignalStore`](SIGNALS.md) — no mutex on
  the hot path, no channels carrying pixels, no allocation per frame.

The render thread never *waits* on the behavior thread. Control flows one way
(render → behavior) through a non-blocking command channel; data flows the other
way (behavior → render) through the triple-buffered store.

## Module map

| Module | Responsibility |
|--------|----------------|
| `engine/` | Owns GPU + source + the editable `PipelineConfig`; orchestrates reload. |
| `runtime/` | Turns a `PipelineConfig` into a running filter chain; per-frame render. |
| `runtime/context.rs` | The addon execution contract: `FilterNode`, `FrameContext`, `SignalContext`, `ResolvedConfig`. |
| `runtime/signals_group.rs` | The `@group(3)` signals uniform (see [GROUP3](GROUP3.md)). |
| `behavior/` | The behavior thread, scheduler, and `BehaviorNode` contract. |
| `signal/` | `SignalValue`/`SignalKind`, `SignalSchema`/`SignalId`, `SignalStore`. |
| `addon/` | Manifests, registry, `pipeline.json`, packaging, compat, errors. |
| `addons/` | The builtin addons (CRT, DotRenderer) + the generic external-shader runner. |
| `effects/` | Shader prelude composition + fullscreen pipeline helper. |
| `ui/` | The egui overlay (edits the config; never touches runtime internals). |

## The one execution interface

Every filter — builtin or external — is a `FilterNode`:

```rust
pub trait FilterNode {
    fn prepare(&mut self, queue: &Queue, signals: &SignalSnapshot) {} // fold signals → uniforms
    fn process(&self, ctx: &mut FrameContext);                        // record the render pass
}
```

The executor in `runtime::render` is uniform: for each node it calls `prepare`
once (upload-only), then `process` (record-only), ping-ponging two GPU targets.
There is **no per-addon branching** anywhere in the executor. A builtin and an
external addon are indistinguishable to it.

## Bind group contract (every filter)

```
@group(0) host context  — resolution + time          (runtime-owned, shared)
@group(1) frame input    — previous node's output / source
@group(2) addon params   — the addon's own uniform (from manifest params)
@group(3) signals        — one vec4 per consumed signal  (ONLY if consume != [])
```

A filter that consumes nothing has no `@group(3)`; its pipeline layout is
byte-identical to a non-consuming addon. See [GROUP3](GROUP3.md).

## The core invariant: `build_count`

Signal-driven animation must **never** rebuild the pipeline. Per frame, signal
updates are `queue.write_buffer` into existing uniforms — no new bind groups, no
pipeline recompilation, no `build()`. `PipelineRuntime::build_count()` increments
only on a structural reload (config edit), and stays constant while signals
oscillate. This is asserted by the smoke tests and visible in the stats line.

## Reload model

A UI edit mutates the in-memory `PipelineConfig` and marks state dirty; after a
short debounce, `Engine::tick_reload` rebuilds. The rebuild **is** the reload —
`build()` validates and atomically swaps the live node list, so a rejected edit
leaves the running pipeline untouched.

* **Filter edits** (param/add/remove/reorder, behavior enable/param) do not
  change the published-signal set → the store and behavior thread keep running;
  only the filter chain rebuilds (or, for behavior hot edits, a live command).
* **Behavior add/remove** changes the schema → the store is recreated and the
  behavior thread takes a **minimal reload** (see [BEHAVIORS](BEHAVIORS.md)):
  unchanged behaviors are reused in place, preserving their resources.

## What is frozen

The architecture, the signal/behavior/filter runtimes, `@group(3)`, the config
format, and the addon manifest schema are **frozen** for v2. New functionality
arrives as addons. Extension points and stability tiers are enumerated in
[FREEZE_REVIEW.md](FREEZE_REVIEW.md).
