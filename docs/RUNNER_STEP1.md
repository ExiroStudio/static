# Runner — Phase 3b, Migration Step 1 (execution seams)

Introduces the addon execution **abstractions** with **no runtime behavior
change**. Everything here is a *seam*: defined, unit-tested, and **dormant** — not
wired into the live behavior path. The engine still creates behaviors through
[`BehaviorHost::create_inits`](../src/behavior/host.rs) and drives them on the
unchanged [`BehaviorScheduler`](../src/behavior/scheduler.rs).

Source of truth: the frozen RFC. Step 1 implements only the seams; subprocess,
protocol, shmem, ABI, sandbox enforcement, capability enforcement, manifest v2,
frames, GPU, and the SDK are **later steps**.

## What landed (`src/runner/`)

| File | Seam | Owns | Does NOT |
|---|---|---|---|
| `backend.rs` | `RunnerBackend` trait | spawn/bind/tick/shutdown *mechanism* | policy, resources, capabilities, transport assumptions |
| `supervisor.rs` | `Supervisor` | watchdog + fault/restart/breaker *policy* | transport, runner internals, execution |
| `host.rs` | `HostApi` trait + `NullHost` | the frozen 13-method capability *surface* | any real capability/transport (stub-safe) |
| `sandbox.rs` | `Sandbox` trait + `Linux/Mac/WindowsSandbox` | the confinement *surface* | enforcement — every `apply` returns `NotImplemented` |
| `inproc.rs` | `InProcessRustRunner` | wraps `BehaviorFactory → BehaviorNode` | **execute** the node — `tick` returns `Idle` |
| `mod.rs` | `RunnerKind`/`RunnerState`/`TickOutcome`/`Handshake`/`RunnerError` | shared types | — |

## Frozen Host API surface (signatures only)

`publish` · `subscribe` · `request_frame` · `change_frame_tier` · `read_frame` ·
`get_param` · `timing` · `log` · `request_resource` · `release_resource` ·
`spawn_worker` · `heartbeat` · `metrics`. `NullHost` implements all of them as
deny/empty/no-op (object-safe via `&mut dyn HostApi`). `metrics()` is read-only.

## Ownership split (frozen)

- **Runner = mechanism.** Six calls: `load → bind → start → tick → stop → unload`.
- **Supervisor = policy.** Lifecycle + fault machine
  (`Loading→Ready→Running⇄Stopped→Unloaded`, `Faulted→Restarting`/`Disabled`),
  exponential capped backoff, breaker on N faults / window T. Pure & deterministic
  (time passed in, never read).
- **HostApi = capability surface.** The only thing an addon ever touches.

## The in-process adapter, precisely

`InProcessRustRunner::load()` calls the `BehaviorFactory` and **holds** the
produced `BehaviorNode` (the "wrap"), reporting its published signals as a
`Handshake`. It **does not execute** the node — `tick()` returns
`TickOutcome::Idle`. The live scheduler remains the sole executor, so behavior
output is byte-for-byte identical. Driving the held node through a `HostApi`-backed
context is the **Step-2 wiring point** (see Known Debt).

`InProcessRustRunner::from_registry(&BehaviorRegistry, &NodeConfig)` is the
compatibility adapter from the Phase-3a factory seam — proving the two compose
with no engine change.

## Guarantees verified

- **Zero behavior diff** — the runner module is dormant (`grep runner:: src/{engine,behavior,runtime}` ⇒ none).
- **Untouched:** scheduler, behavior `node` contract, `SignalStore`, `SignalValue`,
  manifest. Only edit to existing code: `mod runner;` in `main.rs` (+1 line).
- **Warning-clean** `cargo build` and `cargo test`; **101 tests pass** (+14 new),
  all examples still build/validate, `build_count` path unchanged.

## Known debt → Step 2

- `tick()` does not yet drive the node (returns `Idle`). Step 2 builds a
  `HostApi`-backed `BehaviorCtx` and calls `node.update()`.
- `Supervisor` breaker persistence across reload is not wired (it's per-instance
  logic only).
- `Sandbox` backends enforce nothing (return `NotImplemented`) — Step 5.
- Nothing routes the live engine through `RunnerBackend` yet — deliberate.
