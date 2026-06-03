# Behaviors

A **behavior** is a signal *producer*. It runs on the behavior thread (~30 Hz),
may read the latest source frame (CPU only), and publishes signals. It is the
only thing that creates signal data.

## The contract

```rust
pub trait BehaviorNode: Send {
    fn start(&mut self, ctx: &mut BehaviorStartCtx); // resolve ids once; load resources
    fn update(&mut self, ctx: &mut BehaviorCtx);     // every tick: read, publish
    fn stop(&mut self);                              // release resources (reload + shutdown)
}
```

A behavior **cannot** reach the GPU, a queue, a texture, the runtime, or
`build()` — the context types simply do not expose them, so misuse is
unrepresentable rather than merely discouraged.

| Context | Exposes |
|---------|---------|
| `BehaviorStartCtx` | `schema()` (resolve names → `SignalId`), `config()` |
| `BehaviorCtx` | `frame()` (CPU `FrameView`), `publish(id, value)`, `config()`, `timing()` |

`Timing { dt, elapsed }` drives framerate-independent state — advance from `dt`,
not from a tick count.

## Lifecycle

```
spawn ─▶ start_all() ─▶ ┌─ tick: drain commands → update enabled → publish() ─┐ ─▶ stop_all()
                        └────────────── sleep to hold ~30 Hz ──────────────────┘
```

* The scheduler is a single deterministic loop: drain commands at the top of a
  tick (never mid-update), run enabled behaviors in a stable order, then commit
  every staged signal with **one** atomic `publish()` per tick.
* Each tick's `update` pass has an 8 ms budget; exceeding it bumps a stat (and
  warns in debug). The per-tick rate is reported as `behavior_hz`.

## Commands (render thread → behavior thread)

All are non-blocking sends; the render thread never waits.

| Command | Effect | Recreates instance? |
|---------|--------|---------------------|
| `SetParam` | Hot config update | No |
| `Enable` / `Disable` | Toggle participation in `update` | No |
| `Reload` | Structural: new store + schema + behavior set | Minimal diff (below) |
| `Shutdown` | Stop all + join | — |

## Minimal reload (the freeze optimization)

The schema assigns ids in publish order, so **adding** a behavior appends slots
and leaves earlier ids fixed. The scheduler's `Reload` exploits this to diff the
incoming set against the running one, per instance id:

```
for each incoming behavior init:
    matching live instance?
      ├─ yes & every published signal keeps the SAME id under the new schema
      │     → REUSE in place: keep the instance + its loaded resources,
      │       refresh only hot config. No stop/start.
      └─ otherwise (ids moved, or brand-new id)
            → per-instance full reload: stop old (if any), construct fresh, start.
removed-from-set instances → stop().
```

Because resource loading happens in `start` (not construction), a reused
instance never reloads its resources, and a throwaway init for a reused id is
constructed cheaply and dropped without ever starting. Adding a second behavior
therefore leaves the first running untouched. Removing a middle behavior shifts
later ids → those instances take the safe per-instance reload.

## v1 status & the Phase 3 extension point

v1 ships **one** builtin behavior, `time` (`signal.time = sin(elapsed)`), as the
reference producer; the engine constructs builtin behaviors directly. Externally
**loadable** behaviors (scripted/native) are a Phase 3 extension point — they
need executable code, which v1 deliberately does not load. The
[`behavior_time`](../examples/behavior_time) and
[`behavior_counter`](../examples/behavior_counter) examples document the manifest
+ trait shape an external behavior will take.

See [SIGNALS](SIGNALS.md) for the store and [FILTERS](FILTERS.md) for the
consume side.
