# Runner — Phase 3b, Step 4 (HostApi data-plane realization)

Realizes the frozen `HostApi` data-plane **behavior** (Step 2.6) as five host-side
bridges, composed into a real `HostApi` (`BridgedHost`). Changes no frozen
contract; the runner stays **dormant** (no engine wiring); behavior/render/
`build_count`/`SignalStore`/`NativeRunner` unchanged.

## What landed (`src/runner/dataplane/`)

| File | Bridge | Methods | Frozen rule preserved |
|---|---|---|---|
| `signal.rs` | `SignalBridge` | publish / subscribe | overwrite · latest-wins · single atomic commit · snapshot lookup (no queue/stream/listener) |
| `frame.rs` | `FrameBridge` | request_frame / change_frame_tier / read_frame | borrow · tick-pinned · latest snapshot · no copy · no ownership · no GPU |
| `resource.rs` | `ResourceBridge` | request_resource / release_resource | host-owned · opaque handle · lazy + content-addressed cache · idempotent release · epoch-invalidated |
| `metrics.rs` | `MetricsBridge` | metrics | addon-local snapshot only · no engine/cross-addon/stream |
| `worker.rs` | `WorkerBridge` | spawn_worker | runner-owned · host tracks only · depth = 1 (no nested runner) |
| `mod.rs` | `BridgedHost` | **all 13** | composition → a real `HostApi`; snapshot/tick discipline |

`runner/mod.rs`: `mod dataplane;` + additive re-exports.

## Architecture diff

```
runner ─(HostBridge forwards)─▶ BridgedHost : HostApi
                                   ├ SignalBridge   (SignalStore: set→commit→snapshot)
                                   ├ FrameBridge    (host tiers; pinned per tick)
                                   ├ ResourceBridge (opaque handle table + refcount/epoch)
                                   ├ MetricsBridge  (this addon's own metrics)
                                   └ WorkerBridge   (capability + live count)
```

## Data-plane realization

- **Signals.** `publish` stages into a host-owned working frame (overwrite);
  `commit` is one atomic swap (latest-wins). `subscribe` is a stable hashed
  lookup. It *uses* `SignalStore` (never modifies it); only `SignalId`/
  `SignalValue` (POD) leave through the `HostApi`.
- **Frames.** `begin_tick` pins the host's latest frame; `read_frame` returns the
  **same** `FrameRef` for every read that tick; the next tick samples latest-wins.
  Metadata only — no pixel buffer, no ownership, no GPU handle crosses (Step 4
  carries `FrameRef` metadata; per-transport pixel views remain future).
- **Resources.** Lazy, content-addressed by id (same id → same handle, refcounted
  and shared); `release` is idempotent (double/unknown release = no-op); a reload
  bumps an epoch that invalidates all handles. Only the `u32` handle crosses — no
  fd, path, or mmap ownership.
- **Metrics.** One bridge per addon; `metrics()` returns a `Copy` snapshot of *that
  addon's* counters. No engine metrics, no cross-addon visibility, no stream.
- **Workers.** `spawn_worker` is deny-by-default; when granted it hands out opaque
  handles the host *tracks* (the runner owns the worker). depth = 1 is structural
  — the bridge exposes no way to spawn a runner.
- **Snapshot semantics.** `BridgedHost::begin_tick` pins signals + frame for the
  tick; reads are consistent within a tick, latest-wins across ticks.

## Compatibility strategy

- **No frozen-contract edits** (verified): `HostApi` (`host.rs`), typestate
  (`backend.rs`), `Supervisor`, `ExecutionUnit`, `NativeRunner`, `SignalStore`,
  `scheduler`, `node`, `value` are byte-for-byte unchanged.
- **Dormant** — nothing in engine/behavior/runtime/app/ui references the runner;
  `pipeline.json`, signals, UI, examples, `build_count` unaffected.
- **Bridges hold host-side state, expose only POD** — `Arc`/`SignalStore`/handles
  live host-side; the `HostApi` returns only `SignalId`/`SignalValue`/`FrameRef`/
  `ResourceHandle`/`WorkerHandle`/`Metrics`/`ParamValue`/`Timing`.

## Tests (144 total, 143 pass + 1 ignored; 0 warnings)

- publish overwrite + latest-wins across commits; subscribe stable; pinned-snapshot
  consistency.
- multiple `read_frame` return the same frame; latest-wins next tick; lock-free
  tier select.
- resource: shared-handle cache + refcount; idempotent release; epoch invalidation.
- metrics: freshness + copy semantics + per-addon isolation.
- worker: granted/denied + unique tracked handles.
- **end-to-end**: a real `time` behavior, driven through a runner, reaches the
  data plane via `HostApi` and its output appears in the host snapshot.
- **crash isolation**: a crashing `NativeRunner` does not corrupt the host data
  plane (publish/commit/read still correct afterwards).
- **reload stability**: resource handles invalidate through the host.
- Existing suite (behavior/signals/examples/`SignalStore`/render/`NativeRunner`)
  unchanged and green.

## Known debt → Step 5

- **Frame pixel-view transport** — `FrameRef` is metadata only; cross-process
  pixel mapping (shmem/WASM region/remote tile) is still future.
- **No cross-process data marshalling** — the bridges forward in-process; a real
  out-of-process data sub-protocol (needing a protocol child) is future.
- **Metrics source** — `MetricsBridge::update` is host-fed; wiring real cpu/RSS
  collection is later.
- Nothing routes the live engine through `BridgedHost` (deliberate).

## Step 5 readiness

The full host-facing contract is now realized host-side: control (Steps 2.5/3) +
data (this step). `BridgedHost` is a complete `HostApi` a runner drives end-to-end.
**Step 5** can wire a runner + `BridgedHost` into the live engine (and add the
cross-process data marshalling) without changing any frozen contract — exactly the
seam these steps were shaped to admit.
