# Signals

A **signal** is a small, fixed-size value a behavior publishes and a filter
consumes. Signals are the only data crossing the behavior→render boundary.

## Value types

`SignalValue` is `Copy` and heap-free (≤ 20 bytes), so a whole frame of signals
is a flat array copied with one `memcpy`:

| `SignalKind` | Payload | `@group(3)` slot (`vec4<f32>`) |
|--------------|---------|--------------------------------|
| `bool` | `bool` | `.x` (0.0 / 1.0) |
| `f32`  | `f32`  | `.x` |
| `i32`  | `i32`  | `.x` (as f32) |
| `vec2` | `[f32; 2]` | `.xy` |
| `vec3` | `[f32; 3]` | `.xyz` |
| `vec4` | `[f32; 4]` | `.xyzw` |

There are deliberately no strings, vectors, or landmark lists. Rich per-frame
data (if it ever exists) is a later concern; v1 signals are scalars/vectors.

## The schema: name ↔ slot ↔ type

A `SignalSchema` is built once per runtime build from the enabled behaviors'
`publish` declarations (which create slots) validated against the filters'
`consume` declarations (which only reference them).

```
behaviors.publish  ─┐
                    ├─▶ SignalSchemaBuilder ─▶ Arc<SignalSchema>  (names, kinds, by_name index)
filters.consume    ─┘        │
                             ├─ DuplicatePublish  → reject
                             ├─ TypeMismatch      → reject
                             ├─ MissingRequired   → reject
                             └─ optional & missing → warn (fallback)
```

* Ids (`SignalId`) are slot indices, assigned in **publish order**. Stable
  within a build; they may renumber across builds — never cache one across a
  rebuild. Resolve names to ids **once** (at behavior `start` / filter
  instantiate), then address by id on the hot path.
* `SignalSchema::id(name)` is a hashed lookup into a build-time `by_name` index
  — never a linear scan, never an allocation. The index is derived from `names`
  and excluded from schema equality (so the reload path's "did the schema
  change?" comparison is purely structural over names + kinds).

## The store: lock-free triple buffer

`SignalStore` is a single-producer / single-consumer triple buffer. Three
buffers guarantee that the producer and consumer never touch the same buffer at
once (`{write, read, shared}` is always a permutation of `{0,1,2}`):

```
behavior thread          render thread
  set(id, v)  ×N            snapshot_into(&mut snap)   // claim freshest if FRESH bit set
  publish()  ───swap(shared, AcqRel)───▶  reads buffers[read_idx]
   └ writes buffers[write_idx], then atomic swap carries happens-before
```

Guarantees:

* **Atomic frame handoff** — a snapshot can never mix `face.position` from one
  publish with `face.rotation` from another (covered by a 3M-iteration torn-read
  stress test).
* **No allocation on the hot path** — the consumer reuses one snapshot buffer;
  the producer keeps a private working frame so it can update a subset of
  signals without dropping the rest.
* **No mutex, no channels, no `Box<dyn Any>`** on the per-frame path.

## Lifecycle of one signal value

```
behavior.update(ctx)            render frame N
  ctx.publish(id, F32(x))         reader.snapshot_into(&mut snap)   // newest published frame
  …                               for node in nodes: node.prepare(queue, &snap)  // write_buffer → @group(3)
scheduler.publish() (once/tick)   for node in nodes: node.process(ctx)           // shader reads @group(3)
  └ atomic swap → visible
```

Producer ticks at ~30 Hz; the consumer samples whatever the latest published
frame is at ~144 fps (latest-wins; the consumer is never blocked and never sees
a torn frame).

See [BEHAVIORS](BEHAVIORS.md) for the publish side and [GROUP3](GROUP3.md) for
the consume side.
