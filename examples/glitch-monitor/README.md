# Glitch Monitor

The first **external** addon for the Static addon ecosystem — a monochrome
surveillance-glitch filter (horizontal tearing, signal corruption, scan
disruption, block dropout). White-on-black, single pass, no HDR / bloom /
temporal history.

It is packaged as a plain `glitch-monitor.zip` and installed through the UI; it
ships no compiled code — only a manifest, a WGSL shader and a preset asset.

## Package layout

```
glitch-monitor.zip
├── manifest.toml              # identity, compatibility, params, declarations
├── shaders/
│   └── glitch.wgsl            # the effect (composed with the engine prelude)
├── assets/
│   └── presets/heavy.json     # a sample preset (metadata in v1)
└── README.md
```

## Parameters

| Key            | Type | Range     | Default | Meaning                              |
|----------------|------|-----------|---------|--------------------------------------|
| `intensity`    | f32  | 0.0 – 1.0 | 0.6     | Overall strength of the corruption   |
| `frequency`    | f32  | 0.0 – 1.0 | 0.5     | How often disruptions occur          |
| `block_size`   | f32  | 2.0 – 64  | 16.0    | Dropout / displacement block size px |
| `displacement` | f32  | 0.0 – 1.0 | 0.4     | Maximum horizontal tear offset       |

These appear automatically in the properties panel — the UI is generated from
the manifest schema, nothing is hardcoded.

## Compatibility note

This addon targets engine API v1 and is a `pipeline` (filter) addon. It ships no
compiled code: the engine's **generic external-shader runner** loads
`shaders/glitch.wgsl` from the installed directory, composes it with the shared
prelude, and packs the four schema parameters into the `@group(2)` uniform (as
`f32`s, in sorted-key order). Install, discovery, config UI, runtime execution,
reorder, remove and uninstall all work end-to-end.

The runner supports numeric (`f32` / `i32`) parameters; other param types pack
as `0.0`. The shader's `@group(2)` struct must list the params as `f32` fields
in sorted-key order (here: `block_size, displacement, frequency, intensity`).
