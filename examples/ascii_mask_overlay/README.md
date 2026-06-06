# ASCII Mask Overlay

A signal-consuming **filter** that draws a retro-CRT ASCII face (`>_<`) which
follows a tracked face. It is the consumer half of the demo;
[`face_tracking_lite`](../face_tracking_lite) is the producer.

```
@group(1) frame ─▶ sample ─▶ composite procedural >_< (translate/rotate/scale)
@group(3) signals: face.position(.xy), face.rotation(.x), face.scale(.x)
```

## Signals consumed (all optional → fallback hides the overlay)

| Slot | Signal          | Used as                                 |
|------|-----------------|-----------------------------------------|
| 0    | `face.position` | glyph centre (`.xy`, X mirrored for selfie) |
| 1    | `face.rotation` | glyph rotation (`.x`, radians)          |
| 2    | `face.scale`    | glyph size + presence fade (`.x`)       |

Declared `consume` order **is** the `@group(3)` packing order.

## Params (schema-driven Filter panel)

| Key                | Range     | Effect                                            |
|--------------------|-----------|---------------------------------------------------|
| `ascii_expression` | text      | declaration only — v1 renders `>_<` procedurally  |
| `mask_size`        | `0.05..1` | base glyph half-size (screen-height units)        |
| `opacity`          | `0..1`    | overlay strength × presence fade                  |

## Rules honoured

- **ASCII only** — procedural line-segment glyph, **no** Unicode emoji, no font,
  no text rendering system.
- **No rebuild / no bind recreation** — animation is entirely `@group(3)`
  refreshed via `prepare()` (`write_buffer`); `build_count` stays constant.
- **No signal → hide** — with no publisher the slots read 0 → `scale = 0` →
  presence fade is 0 → pure passthrough.

Single-expression mode by design: no animation, no expression switching, no state
machine. Place it **last** in the pipeline so it composites on top of the image.
