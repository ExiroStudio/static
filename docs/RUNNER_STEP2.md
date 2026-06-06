# Runner — Phase 3b, Migration Step 2 (mock execution flow)

Validates the execution seams by proving, **in-process and test-only**, that
`BehaviorFactory → ExecutionUnit → RunnerBackend → HostApi → BehaviorNode.update()`
works — with the four Step-1 refactors applied. No subprocess, protocol, shmem,
sandbox execution, manifest change, or behavior migration. The runner stays
**dormant** (no live engine reference); behavior output and `build_count` are
unchanged.

## Step-1 feedback applied

| # | Feedback | Resolution |
|---|----------|------------|
| **F1** | Don't store `BehaviorNode` in the runner | `ExecutionUnit` trait; the node lives in `BehaviorExecutionUnit`. Runners own `Box<dyn ExecutionUnit>` only — a native unit (no node) fits the same surface. |
| **F2** | Keep the `Handshake` tiny | `Handshake { version, caps, publish: Vec<SignalId>, consume: Vec<SignalId> }`. Ids only — no schema/specs/payload/frame metadata. Old `Vec<SignalSpec/Ref>` removed. |
| **F3** | No real clock in policy | `Clock` trait + `ManualClock`/`SystemClock`. `Supervisor` reads time only via the injected clock; tests are deterministic. |
| **F4** | `Loaded→Bound→Started` states | Typestate: `LoadedRunner → BoundRunner → RunningRunner → StoppedRunner`. Illegal transitions are **type errors** (e.g. `LoadedRunner` has no `tick`), not runtime checks. |

## Execution flow

```
Box<dyn RunnerBackend>.load()          ─▶ LoadedRunner     (unit built; nothing run)
                      .bind(host)        ─▶ BoundRunner      (Handshake: version+caps+ids)
                      .start(host)       ─▶ RunningRunner    (ExecutionUnit::start)
                      .tick(host,timing) ─▶ TickOutcome      (ExecutionUnit::run → node.update → host.publish)
                      .stop(host)        ─▶ StoppedRunner    ─▶ start | unload
```

`BehaviorExecutionUnit` drives a real `BehaviorNode`: it owns a private,
single-node `SignalStore` as the publish buffer (the node only speaks
`BehaviorCtx`), runs `node.update()`, then mirrors each published slot onto the
`HostApi`. It **uses** the signal types; it modifies none of them and never
touches the live scheduler.

## What landed (`src/runner/`)

- `execution.rs` — `ExecutionUnit` trait; `BehaviorExecutionUnit` (real node);
  `MockExecutionUnit` (controllable fake: records lifecycle, exercises the host
  surface, can fault).
- `backend.rs` — `RunnerBackend::load` + typestate `Loaded/Bound/Running/Stopped`.
- `clock.rs` — `Clock` + `ManualClock` + `SystemClock`.
- `mock.rs` — `MockRunnerBackend` (from a factory → real node, or a prebuilt unit).
- `host.rs` — `RecordingHost` added (records publish/heartbeat/request_frame,
  resolves subscribe via an optional schema). **`NullHost` remains valid.**
- `supervisor.rs` — clock injected.
- `mod.rs` — slim `Handshake`, `Capabilities`, `ABI_VERSION`, re-exports.

## Host API consumption proven

`publish`, `subscribe`, `timing`, `metrics`, `request_frame` are all consumed from
a unit (via `MockExecutionUnit`/`BehaviorExecutionUnit` against `RecordingHost`).
`NullHost` still satisfies the full surface (deny-by-default) and drives the flow.

## Tests (107 total, 106 pass + 1 ignored; 0 warnings)

- Execution lifecycle + `Loaded→Bound→Started` typestate flow.
- **Illegal transitions are compile-time** — there is no `tick` on a `LoadedRunner`
  (typestate), so it cannot be expressed; documented rather than runtime-asserted.
- Clock injection (deterministic `ManualClock`: backoff, breaker, window reset).
- Host binding (handshake ids resolved via the host).
- Supervisor timing (fault → restart/disable under a manual clock).
- Behavior compatibility: the in-process runner reproduces `signal.time = sin(elapsed)`
  exactly (identical output).
- Zero render/behavior diff: runner dormant; frozen core (`scheduler`, `node`,
  `store`, `value`, `manifest`) untouched; `build_count` path unchanged.

## Known debt → Step 3

- No `RunnerBackend` is wired into the live engine (deliberate).
- `Supervisor` is not yet attached to a runner's tick loop (it's standalone policy).
- `Sandbox` still enforces nothing; `Capabilities` is a declaration only.
- `Handshake` ids are mock-resolved via `host.subscribe`; real publish-id assignment
  is Step 3 wiring.
- `BehaviorExecutionUnit`'s private store is the in-process bridge; the native unit
  (subprocess/shmem) is Step 3.
