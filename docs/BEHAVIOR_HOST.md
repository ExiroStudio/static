# Behavior Host (Phase 3a)

The seam that lets an **external behavior addon execute** without a per-addon
engine edit. Before this, the render thread mapped a `pipeline.json` behavior
entry to an instance with a hardcoded `match node.addon { "time" => … }` — every
new producer meant editing the engine. The host replaces that with a registry
lookup.

```
manifest (on disk)  ──register_behavior_with──▶  BehaviorRegistry
pipeline.json entry ──BehaviorHost::create_inits──▶  BehaviorInit ──▶ scheduler
```

## The three pieces

| Piece | Where | What |
|-------|-------|------|
| `BehaviorFactory` | `behavior/host.rs` | `fn(String, ParamMap, bool) -> BehaviorInit` — a type alias; the same shape as the existing `init_with` constructors. |
| `BehaviorRegistry` | `behavior/host.rs` | `id → factory` map. Registration is the only way in. |
| `BehaviorHost` | `behavior/host.rs` | `create_inits(registry, &[NodeConfig])` — resolves a config's behavior set to runnable inits **by lookup**. Unknown ids are skipped, not faked. |

`PipelineRuntime` owns the registry and exposes:

* `register_behavior_with(manifest, factory)` — bind a factory to an id, and (unless
  a scanned on-disk package already provided it) register the manifest for the UI.
* `register_behavior(manifest)` — **kept**: the manifest-only path for a
  non-executable reference producer (no factory ⇒ not runnable).
* `behavior_registry()` — the lookup the engine resolves through.

## What did *not* change

`BehaviorNode` (`start`/`update`/`stop`), the scheduler (30 Hz, atomic publish,
minimal-diff reload), the `SignalStore`, the render path, and the UI are
**byte-for-byte unchanged**. The engine edit is ~33 net LOC across
`engine/mod.rs`, `runtime/mod.rs`, and `behavior/mod.rs`.

## Honest scope (and Phase 3b)

v1 loads **no** scripting, WASM, or dynamic library. A factory's code is therefore
compiled into the binary; the addon is "external" in **packaging** (its manifest +
config ship in `examples/<id>/`, installed to `addons/<id>/`) and bound to its id
through the registry. This removes the *per-addon dispatch coupling* — the goal of
the seam — without claiming data-only execution the constraints forbid.

**Phase 3b** swaps in a real out-of-process backend (cdylib via `libloading`,
WASM, or a script host) **behind the same `BehaviorFactory` signature**. Nothing
above changes; only what *populates* the factory does. The seam was shaped to
admit that without redesign.

## Registration order

Scan `addons/` **before** `register_behavior_with`, so a compiled factory can
attach to a package discovered on disk (the package then owns the UI param
schema; the factory supplies only execution). The engine does this in
`Engine::new`.

See [BEHAVIORS](BEHAVIORS.md) for the producer contract and
[FACE_TRACKING_LITE](FACE_TRACKING_LITE.md) for the first addon to use this seam.
