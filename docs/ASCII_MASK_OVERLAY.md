# AsciiMaskOverlay

The consumer half of the demo: a **signal-consuming filter** that draws a retro
CRT ASCII face (`>_<`) which follows the tracked face. A pure external addon
(manifest + WGSL, no compiled code) — it runs through the engine's generic
external-shader runner, identical in shape to the builtin CRT.

* Package: `examples/ascii_mask_overlay/` (canonical) → `addons/ascii_mask_overlay/` (install)
* Shader: `shaders/ascii_mask.wgsl`

## Signals consumed (`@group(3)`)

Declared `consume` order **is** the slot order:

| Slot | Signal          | Read as | Drives                                   |
|------|-----------------|---------|------------------------------------------|
| 0    | `face.position` | `.xy`   | glyph centre (X mirrored for the selfie) |
| 1    | `face.rotation` | `.x`    | glyph rotation (radians)                 |
| 2    | `face.scale`    | `.x`    | glyph size **and** the presence fade     |

All three are `optional`: with no publisher they read the **0.0 fallback**, so
`scale = 0` → presence fade is 0 → the overlay is fully transparent (the
"no signal → hide" rule, done as a smooth fade).

## Transform & rendering

```
fragment uv → translate(center) → aspect-correct → rotate(face.rotation)
            → scale(1 / (mask_size · mix(0.5,2.0,face.scale)))
            → procedural ">_<" via line-segment SDF → composite over input
```

The `>_<` is three pairs/one of line segments (`>` `_` `<`), thresholded to
strokes, plus a thin surveillance tracking box and CRT scanline/flicker
modulation. No font, no glyph atlas, no text system — and **ASCII only**, never a
Unicode emoji.

## Params (schema-driven Filter panel)

| Key                | Default | Range     | Effect                                       |
|--------------------|---------|-----------|----------------------------------------------|
| `ascii_expression` | `">_<"` | text      | declaration only — v1 is single-expression, rendered procedurally; switching is intentionally excluded |
| `mask_size`        | 0.25    | `0.05..1` | base glyph half-size (screen-height units)   |
| `opacity`          | 0.9     | `0..1`    | overlay strength × presence fade             |

`@group(2)` packs numeric params in **sorted-key** order — `[ascii_expression,
mask_size, opacity]` — which the shader's `Params` struct mirrors (the text param
packs as `0.0`).

## Invariants

* **No rebuild / no bind recreation** — animation is entirely `@group(3)`,
  refreshed each frame via `prepare()` (`write_buffer`). `build_count` stays
  constant while the face moves; recompilation never happens. This is the same
  path the freeze proved for CRT and the external examples.
* **Size-independent** — reads resolution from `@group(0)`, survives a resize.

Place it **last** in the pipeline so it composites on top. See
[FILTERS](FILTERS.md) and [GROUP3](GROUP3.md) for the consume contract.
