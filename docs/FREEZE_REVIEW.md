# Engine v2 — Freeze Review

Final hardening pass before freezing Engine v2. After this point, new
functionality should arrive through **addons**, not engine edits.

## Verdict

> **FREEZE: APPROVED.** All acceptance criteria met. The engine is stable enough
> that an addon author can build a signal-consuming **filter** end-to-end with no
> engine modification. The one carve-out is externally *loadable* behaviors,
> which are an explicit Phase 3 extension point (v1 ships the builtin `time`
> producer + the `BehaviorNode` contract as the reference).

| Metric | Value |
|--------|-------|
| Render rate | ~144 fps (unchanged) |
| `build_count` while signals animate | constant (no rebuild loops) |
| Tests | **70 passing**, 1 ignored (release-only benchmark) |
| Build warnings (`cargo build` / `cargo test`) | **0** |
| Example addons | 4, all manifests validate + shaders compile |
| Source size | ~8.2k LoC across `src/` |
| Render path | unchanged |
| Behavior runtime contract | unchanged (30 Hz, deterministic, atomic publish) |

## What changed in this pass

| Task | Change | Risk |
|------|--------|------|
| 1 — External shader consume | The generic external-shader runner now builds `@group(3)` and refreshes it per frame when an addon declares `consume`, identically to the builtin CRT. Empty consume ⇒ byte-identical layout to before. | Low — reuses the existing `SignalsBinding`; gated on `consume != []`. |
| 2 — Signal id interning | `SignalSchema::id()` is now a hashed `by_name` lookup built once at finalize, not a linear scan. Index excluded from equality (reload comparison stays structural). | Low — equality semantics preserved by a manual `PartialEq`. |
| 3 — Reload diff | The behavior scheduler's `Reload` reconciles per instance id: reuse-in-place when published ids are stable (preserving resources), per-instance full reload otherwise, stop removed. | Medium — internal to the scheduler; covered by 3 reload tests; observable contract unchanged. |
| 4 — Examples | `behavior_time`, `behavior_counter`, `filter_signal_consume`, `external_shader_signal`. | None — additive. |
| 5 — Docs | 7 docs (ARCHITECTURE, SIGNALS, BEHAVIORS, FILTERS, GROUP3, CONFIG_V2, ADDON_GUIDE) + this review. | None. |
| 6 — Validation | GPU-free smoke harness (`src/smoke.rs`): publish→consume schema, group3 order, reload survival, config load, external consume, examples build. | None — test-only. |
| 7 — API polish | Removed dead code (`NotFound`, `shader_path`/`asset_path`, `addon_in_use`, `duplicate_node`, `package_filename`/`PACKAGE_EXTENSION`/`manifest_from_dir`, `ids`/`engine_api`); trimmed the `addon` facade to `AddonError`/`Result`; gated intentional future-UI surface with documented `allow(dead_code)`. | Low — none of the removed items had non-test callers. |

## Stability tiers

### Stable (frozen — the contracts addons depend on)
* **Signal runtime** — `SignalValue`/`SignalKind`, `SignalSchema`/`SignalId`,
  `SignalStore` triple buffer, atomic snapshot.
* **`FilterNode`** contract + `FrameContext` + the `@group(0..3)` bind-group
  layout, including `@group(3)` packing (one `vec4` per consumed signal, in
  declared `consume` order).
* **`BehaviorNode`** contract + `BehaviorStartCtx`/`BehaviorCtx` + 30 Hz
  deterministic scheduler semantics.
* **Manifest schema** (`manifest_version = 1`): identity, api range, kind,
  params (`ParamSpec`/`ParamValue`), `publish`/`consume`, shaders/assets.
* **`pipeline.json`** (`format_version = 2`) + its validation + the `Engine`
  editing API.
* **Generic external-shader runner**: prelude composition, sorted-key `@group(2)`
  packing, `@group(3)` from `consume`.

### Experimental / reference-only (shape frozen, wiring is Phase 3)
* **Externally loadable behaviors** — manifest/trait shape is fixed and
  documented; v1 does not execute external behavior code (no scripting/native).
* **`RejectedAddon` surfacing**, registry listing accessors (`rejected`, `len`,
  `contains`) — designed for a future addon-management UI; present, not yet wired.
* **Asset declarations** (`[[assets]]`) — parsed/validated; consumption beyond
  presets-as-metadata is future.

### Internal (not an extension point; may change freely)
* Triple-buffer indexing, scheduler slot bookkeeping, ping-pong target
  management, reload debounce, stats/diagnostics, GPU context setup, the egui
  input bridge.

## Public extension points (the freeze surface)

1. **Filter addon** — ship a manifest + WGSL; read `@group(2)` params and
   `@group(3)` signals. Fully supported, runs through the same path as builtins.
2. **Signals** — declare `consume` (filter) / `publish` (behavior) names+kinds;
   the schema wires producers to consumers with validation + optional fallback.
3. **Behavior addon (contract)** — implement the producer shape; runnable as a
   builtin today, externally loadable in Phase 3.
4. **Pipeline composition** — `pipeline.json` (add/remove/reorder filters,
   add/remove/toggle behaviors, per-instance config).

An author can build **Filter → (consume) Signal → published by a Behavior** with
no engine change; the only engine-side dependency is that the *behavior* be a
builtin (or arrive via Phase 3 scripting).

## Technical debt remaining

* **External behaviors are not executable** — the single real gap vs. the
  "Behavior → Signal → Filter without engine modification" ideal. Deliberate:
  scripting/wasm/native are out of v1 scope.
* **GPU paths are not covered by automated tests** — `prepare`/`process` and
  `SignalsBinding` GPU resource creation are exercised only at runtime (CI is
  headless). The CPU halves (packing order, schema, reload) are smoke-tested.
* **Clippy stylistic lints remain** (collapsible `if`, >7-arg builders, a
  `% 4` vs `is_multiple_of`) — pre-existing in character; `cargo build`/`test`
  are warning-clean. Not freeze-blocking.
* **One source kind / one sink kind** (`webcam`/`window`) — sources/sinks are
  intentionally engine-shipped and not yet pluggable.

## MVP status

**MVP complete.** Source → filter chain → sink runs at 144 fps; behaviors
publish at 30 Hz; filters consume live signals through `@group(3)` with no
rebuild; the pipeline is editable and hot-reloads; external addons install,
validate, run, reorder, and uninstall — including signal-consuming ones.

## Phase 3 scope (NOT in this freeze)

Out of scope and intentionally not built: face tracking, native addons, wasm /
scripted behaviors, a marketplace, a graph editor. The natural first Phase 3
item is an **execution host for external behaviors** (so producers join the
ecosystem on the same footing as filters), followed by richer signal payloads
and additional dynamic `@group(3)` bindings (atlases, history buffers) — all of
which the current contracts were shaped to admit without redesign.
