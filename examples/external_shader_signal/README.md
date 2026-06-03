# external_shader_signal (example)

A richer external filter than `filter_signal_consume`: it warps the frame with a
`signal.time`-driven horizontal wave and exposes **two** params. Where the pulse
example is the minimum, this is the shape a real signal-reactive effect takes.

## What it demonstrates

- **Multiple params** — `amount`, `softness`. The runner packs numeric params
  into `@group(2)` in **sorted-key order**, so the shader struct must list them
  alphabetically (`amount`, then `softness`) and pad to a 16-byte multiple.
- **A consumed signal** — `signal.time` at `@group(3)` slot 0, animating the
  wave phase. No rebuild, no recompilation: only the uniform bytes change.
- **Optional + fallback** — with no publisher, the slot reads `0.0` and the wave
  freezes; the effect still loads and runs.

## Parameter ↔ struct mapping

| Manifest key | Sorted index | Shader field |
|--------------|--------------|--------------|
| `amount`     | 0            | `P.amount`   |
| `softness`   | 1            | `P.softness` |
| (pad)        | 2, 3         | `_p0`, `_p1` |

Get this order wrong and the params silently land in the wrong fields — it is
the one manual contract the generic runner cannot check for you.

## Run it

Install, then `webcam → external-shader-signal → window` with the `time`
behavior enabled. The wave animates from the signal alone; `build_count` stays
constant. See [docs/GROUP3.md](../../docs/GROUP3.md) and
[docs/ADDON_GUIDE.md](../../docs/ADDON_GUIDE.md).
