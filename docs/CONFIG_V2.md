# Config v2 — `pipeline.json`

`pipeline.json` is the single source of truth for a running setup. It is a
portable JSON document (tooling-generated, tooling-consumed) describing one
complete pipeline. The engine loads it at startup and persists edits back to it.

## Shape

```json
{
  "format_version": 2,
  "name": "Default — surveillance dots",
  "source": { "type": "webcam", "config": {} },
  "pipeline": [
    { "instance_id": "node-dot", "addon": "dot-renderer", "enabled": true,
      "config": { "cell_size": 9.0, "mirror": true } },
    { "instance_id": "node-crt", "addon": "crt", "enabled": false,
      "config": { "scanline": 0.22 } }
  ],
  "behaviors": [
    { "instance_id": "beh-time", "addon": "time", "enabled": true, "config": {} }
  ],
  "sink": { "type": "window", "config": {} }
}
```

| Field | Meaning |
|-------|---------|
| `format_version` | `2`. v1 files (no `behaviors`) migrate forward on load. |
| `source` / `sink` | Engine-shipped, **not** addons. v1: `webcam` / `window`. |
| `pipeline` | **Ordered** filter chain (render thread). |
| `behaviors` | **Unordered** producer set (behavior thread). New in v2. |
| `instance_id` | Unique across filters *and* behaviors; the same addon may appear many times. |
| `addon` | The `id` field of an installed addon's manifest. |
| `enabled` | Disabled nodes are skipped at build (filters) / `update` (behaviors). |
| `config` | Per-instance param values; validated against the addon's `ParamSpec`. |

## v1 → v2 migration

A pre-v2 document has no `behaviors` key. On `load` it deserializes with an
empty behavior set (serde default), then `format_version` is stamped to the
current version so a subsequent `save` writes v2. Unknown/future versions are
rejected.

## Validation (two stages)

1. **Structural** (`validate_structure`) — version in range; `instance_id`s
   non-empty and unique across filters + behaviors; source/sink types present.
   Does not consult the registry.
2. **Against the registry** (`validate_against`) — every referenced addon is
   installed and every config value satisfies its addon's `ParamSpec`. Returns a
   *list* of issues (never throws) so the UI can show them all at once:
   `AddonNotInstalled`, `UnknownParam`, `InvalidParam`.

A rejected build leaves the previously-running pipeline live — see the reload
model in [ARCHITECTURE](ARCHITECTURE.md).

## Editing API (on `Engine`)

The UI mutates the document through a thin API and never touches runtime
internals: `set_param`, `set_enabled`, `add_node`, `remove_node`, `move_node`
(filters); `add_behavior`, `remove_behavior`, `set_behavior_enabled`,
`set_behavior_param` (behaviors). Continuous edits (slider drags) debounce;
discrete edits (toggles) apply next frame. Behavior enable/param edits are
**hot** (a live command, no rebuild).

## Addon parameter schema

Each addon declares its params in its manifest; the value type is `ParamValue`
(`Bool`/`I32`/`F32`/`Str`/enum) and the spec is `ParamSpec` (with range / enum
values / UI hints). The properties panel is generated from this schema —
nothing about a param is hardcoded in the engine. See [ADDON_GUIDE](ADDON_GUIDE.md).
