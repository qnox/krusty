# FQN Name Tree Migration

Status: the core migration has since LANDED (`src/name_tree.rs` — the lock-free `NameTree` with
`NameId`/`TypeName` handles — plus the follow-up rounds that moved resolver, classpath, and signature
owners onto it). This document records the design constraint the implementation follows, the measured
negative result that shaped it, and what remains.

## Design constraint (holds in the landed implementation)

The name table owns no resolver semantics. Names are stored as segment paths; package vs nested-class
meaning is established later by resolution. The compact representation stores a path such as
`kotlin/collections/Map$Entry` without deciding where the package ends — when a resolved class
identity needs that split, the resolver/classpath layer stores it alongside the handle. There is no
separate class arena inside the name table: class identity is a resolver/classpath concern, not a
property of raw name storage.

## Measured result that shaped the design: a shallow migration regresses

The first experiment replaced only the `ClassNames` values with a compact path id while leaving most
existing owners as `String`. That REGRESSED both memory and time: the compiler still retained strings
at most boundaries while paying render/cache overhead whenever a compact name crossed into a string
API.

Compile-only sampled corpus (`KRUSTY_BOX_LIMIT=1000`, `KRUSTY_NO_RUN=1`, `KRUSTY_MEM_REPORT=1`,
`KRUSTY_TEST_THREADS=1`; scanned 1051, krusty-compiled 375, FAIL 0):

| Variant | End-of-run RSS | Wall time |
| --- | ---: | ---: |
| Base `d8e9ca7e` | 346 MiB | 158.7s warmed |
| Partial `ClassNames` path table, hash-map edge lookup | 361 MiB | 173.1s warmed |
| Partial `ClassNames` prefix tree | 361 MiB | 173.6s |

(The harness reports `VmRSS` after the compile loop — a retained-memory proxy, not a max-RSS sample.)

Lesson: convert an owner SLICE at a time, but only ship once the dominant long-lived `String` owners
in that slice are gone — a conversion that leaves the strings alive adds boundary overhead for
nothing. The landed migration followed this: resolver/import/type-alias owners, `Ty` object
internals, `ClassSig` identity fields (`internal`, `inner_of`, `interfaces`), and the classpath
type/extension indexes all hold `TypeName` handles today, and the combined rounds took the
full-corpus check from ~1.5 GiB peak RSS to under 400 MiB.

## Remaining (not yet migrated)

- Simple MEMBER names (method/property map keys in `ClassSig`, `Signature` names) are still
  `String`s. These are single segments, not dotted paths — if they ever show up in allocation
  attribution, they want a plain string interner, not the path tree.
- Rendered-string caches at the JVM boundary are per-edge and keyed by handle; keep it that way —
  a global render cache would recreate the retained-string problem the experiment measured.

## Gate for any future slice

- Run the compile-only conformance memory command (`KRUSTY_MEM_REPORT=1` variant above).
- For allocation attribution, rerun a smaller sample with `--features dhat-heap` and
  `KRUSTY_NO_RUN=1`.
- Do not merge a slice unless RSS is neutral or lower and conformance stays `FAIL: 0`.
