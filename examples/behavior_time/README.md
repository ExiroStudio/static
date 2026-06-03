# behavior_time (example)

The smallest possible **behavior** addon: it publishes one signal,
`signal.time = sin(elapsed)`, every behavior tick (~30 Hz). It is the literal
reference for the engine's builtin `time` producer.

## What a behavior is

A behavior runs on the dedicated behavior thread. It may read the latest source
frame (CPU only) and publishes signals; it can never touch the GPU, the queue,
the runtime, or trigger a rebuild — the context types simply do not expose them.

## Lifecycle

```
start(ctx)   — resolve published signal names → SignalId once; load resources
update(ctx)  — every tick: read timing/frame, publish signal values
stop()       — release resources (called on reload + shutdown)
```

The Rust shape (see the builtin `src/behavior/builtins/time.rs`):

```rust
fn start(&mut self, ctx: &mut BehaviorStartCtx) {
    self.time_id = ctx.schema().id("signal.time"); // resolve once
}
fn update(&mut self, ctx: &mut BehaviorCtx) {
    if let Some(id) = self.time_id {
        ctx.publish(id, SignalValue::F32(ctx.timing().elapsed.sin()));
    }
}
fn stop(&mut self) { self.time_id = None; }
```

## v1 status

In v1 behaviors ship as engine builtins (the only runnable producer is `time`).
This directory documents the manifest + trait shape; externally-loadable
behaviors (scripted/native) are a Phase 3 extension point. See
[docs/BEHAVIORS.md](../../docs/BEHAVIORS.md).
