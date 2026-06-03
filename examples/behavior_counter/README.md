# behavior_counter (example)

A **stateful** behavior reference: it holds an accumulator and publishes
`counter.value` (f32), advancing by `rate · dt` each tick. Where `behavior_time`
is stateless, this shows the two things a real producer adds: per-instance state
and a configurable parameter.

## Sketch

```rust
struct Counter { id: Option<SignalId>, value: f32 }

fn start(&mut self, ctx: &mut BehaviorStartCtx) {
    self.id = ctx.schema().id("counter.value");
    self.value = 0.0;                              // state lives in the instance
}
fn update(&mut self, ctx: &mut BehaviorCtx) {
    let rate = ctx.config().f32("rate");           // hot config (SetParam-safe)
    self.value += rate * ctx.timing().dt;          // advance with frame dt
    if let Some(id) = self.id {
        ctx.publish(id, SignalValue::F32(self.value));
    }
}
fn stop(&mut self) { self.value = 0.0; }
```

## Why `dt`, not tick count

`update` receives `Timing { dt, elapsed }`. Driving state from `dt` keeps a
behavior framerate-independent, so the published value is the same whether the
behavior thread runs at 30 Hz or briefly stalls.

## Hot config

Editing `rate` in the UI sends a `SetParam` to the behavior thread; the running
instance picks it up next tick **without** being recreated — its accumulator is
preserved. See [docs/BEHAVIORS.md](../../docs/BEHAVIORS.md) and
[docs/SIGNALS.md](../../docs/SIGNALS.md).

## v1 status

As with `behavior_time`, externally-loadable behaviors are Phase 3; this is a
manifest/contract reference, not a drop-in runnable addon in v1.
