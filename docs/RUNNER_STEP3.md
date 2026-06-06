# Runner — Phase 3b, Step 3 (NativeRunner transport realization)

Realizes a subprocess-backed runner behind the **frozen** seam. Changes nothing in
the typestate, `RunnerBackend`, `ExecutionUnit`, `HostApi`, `Supervisor`,
`Handshake`, `TickOutcome`, or `SignalStore`. The runner is still **dormant** (the
live engine never references it); behavior/render/`build_count` unchanged.

## What landed (`src/runner/native/`)

| File | Component | Role |
|---|---|---|
| `mod.rs` | `ControlRequest`/`ControlResponse`/`TransportError` + **protocol-adapter codec** | the 8 control messages as bytes (control only) |
| `transport.rs` | `ControlTransport` trait + **`LoopbackTransport`** | deterministic in-process child: full protocol, legality, fault |
| `process.rs` | **`NativeProcess`** + **`ProcessTransport`** | one real OS child: spawn, write, liveness, crash detect, reap |
| `bridge.rs` | **`HostBridge`** | forwarding `HostApi` proxy — runner forwards, owns nothing |
| `supervisor.rs` | **`ProcessSupervisor`** | frozen `Supervisor` + respawn glue (crash → Fault → restart/disable) |
| `backend.rs` | **`NativeRunnerBackend`** + `NativeExecutionUnit` | the `RunnerBackend`/`ExecutionUnit` realization |

`runner/mod.rs`: `mod native;` + re-exports + the reserved `RunnerKind::Native`
(additive; diagnostics only).

## Architecture diff

```
NativeRunnerBackend ─load─▶ LoadedRunner(NativeExecutionUnit)   [metadata only; no spawn]
   bind(host)  ─▶ forwards subscribe → host via HostBridge
   start(host) ─▶ spawn child + control handshake (Load·Bind·Start)   [heavy init]
   tick(host)  ─▶ ControlTransport.request(Tick) → TickOutcome        [budgeted, never blocks]
   stop(host)  ─▶ Stop·Unload, terminate + reap
ProcessSupervisor: frozen Supervisor + respawn (FaultDecision → Restart/Disable)
```

## Transport realization

- **Control plane only.** Exactly `Load/Bind/Start/Tick/Stop/Unload` cross, as
  request→response. Heartbeat is implicit (a successful response); Fault is a
  `TransportError` (closed/timeout/protocol). No stream, no event bus.
- **Data plane never enters the transport** (frozen). The `HostBridge` forwards
  `publish/subscribe/read_frame/…` to the real host **in-process**; no signal/
  frame/resource/metric bytes cross the control channel.
- **Two transports, one contract.** `LoopbackTransport` (in-process) proves the
  protocol, the Step-2.5 legality matrix, start-failure, and mid-run crash
  deterministically. `ProcessTransport` owns a real OS child and proves spawn,
  write-to-child, crash detection, and reap.
- **Lifecycle mapping (frozen rules):** `load` = metadata only; `bind` = host
  binding; `start` = spawn + handshake (heavy); `tick` = one control round-trip;
  `stop` = graceful then forced reap. Start failure surfaces on the first tick as
  `Faulted` (Step-2.5 reconciliation — typestate `start` stays infallible).

## Compatibility strategy

- **No frozen-contract edits.** `Supervisor`, `HostApi`, typestate, `TickOutcome`,
  `Handshake`, `SignalStore` are byte-for-byte unchanged (verified via `git diff`).
- **Dormant.** Nothing in `engine`/`behavior`/`runtime`/`app`/`ui` references the
  runner. `pipeline.json`, signals, config, UI, examples are unaffected;
  `build_count` path unchanged.
- **Runner owns process lifecycle only** — never signals/frames/resources/metrics;
  no local cache, no shadow state. The `HostBridge` forwards 1:1.

## Tests (125 total, 124 pass + 1 ignored; 0 warnings)

- Codec round-trip + malformed-frame rejection (no panics).
- Loopback: full lifecycle, legality enforcement, mid-run crash, start failure.
- Real process (unix-gated): spawn + reap; dead child → `Closed`; live child
  accepts a control frame.
- Native lifecycle over loopback **and** a real `sh` child.
- `HostBridge`: forwards publish/subscribe/heartbeat/request_frame to the real
  host; forwarding is identical to calling the host directly.
- `ProcessSupervisor`: crash → Fault → restart ×2 → breaker Disable (deterministic
  via `ManualClock`); healthy runner runs clean.
- Existing suite (behavior/signals/examples/`build_count`) unchanged and green.

## Known debt → Step 4

- **No protocol-speaking child binary** (no SDK/packaging in scope). So
  `ProcessTransport` **synthesizes** the control response from liveness; parsing a
  real child reply needs that child to exist. The full real-process protocol
  round-trip is therefore proven *by halves* (loopback = protocol; process =
  spawn/crash/reap), not end-to-end.
- **Data-plane transport** (publish/frames/resources crossing a process boundary)
  is still in-process forwarding — the cross-process data sub-protocol is future.
- **SIGTERM grace window** needs `libc`; `terminate` currently uses SIGKILL+reap.
- `ProcessTransport` does not read child stdout (synthetic acks) → no blocking-read
  machinery yet.

## Step 4 readiness

The control seam is realized: a `NativeRunnerBackend` runs through the frozen
typestate, faults route through the frozen `Supervisor`, and the `HostBridge`
holds the data-plane forwarding contract. Step 4's job is the **data-plane
transport + a protocol-speaking child** (and, with them, wiring a runner into the
live engine) — none of which requires changing the contracts frozen in Steps 2–2.6.
