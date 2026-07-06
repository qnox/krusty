# Lazy classpath resolution

## Motivation

Name resolution is import-driven: to resolve a referenced name it probes `resolve_type` /
`SymbolSource::functions` once per *(name × default-import package)* — `kotlin.*`,
`kotlin.collections.*`, `kotlin.text.*`, … Profiling the box conformance compile
(`KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1`) shows `collect_signatures_with_cp` at **~65% of compile**,
dominated by that probing plus the eager extension-index scan. Under branch-coverage instrumentation
this amplifies ~9×, and the pre-push coverage gate runs tens of minutes.

The eager simple-name class index that used to make this O(1) was removed for memory (it eagerly
loaded the whole ~30k-class JDK, ~85 MB). This spec resolves FQNs and functions lazily with **near-zero
eager work** and **no full-jar scans**, so both memory and compile time stay low.

## Principles

- Nothing is parsed eagerly except one **shallow `kotlin_module` read per jar**.
- A class is found by FQN with an **O(1) zip `by_name` lookup** — no name catalog.
- Facade statics, `.kotlin_builtins`, and class `@Metadata` members are parsed **lazily, per requested
  package / class**, and cached.
- Everything is **per-jar (per classpath entry)** and **composed per classpath** — a cp that adds one
  library reuses every other jar's cached data.

## Kotlin descriptors used (JVM jar)

| Descriptor | Scope | Use |
|---|---|---|
| `@Metadata` (annotation inside each `.class`) | per class | members, signatures — lazy per class |
| `META-INF/*.kotlin_module` (protobuf, ~8 KB) | per jar | `package → [multifile-facade part class names]` — the only eager read |
| `<pkg>/<pkg>.kotlin_builtins` (protobuf) | per package | builtin types with no `.class` (List, Int, Map…) — lazy per package |

## Data structures

### Per jar — `JarPackages` (cached by jar path via `EntryCache<T>`)
Built lazily on first touch of the jar, from **one shallow `kotlin_module` parse** plus a
central-directory **package-name pass** (entry names only — no decompression, no class parse):
- `packages: HashMap<pkg, PkgEntry>`
- `PkgEntry { facades: Vec<FacadePartName> /* from kotlin_module */, has_classes: bool /* dir has p/*.class */, has_builtins: bool }`

### Package tree — composed per classpath (cached on the per-thread `Classpath`)
```
PackageNode { children: HashMap<segment, PackageNode>, jars: Vec<JarId> }
```
`node.jars` = every jar whose `JarPackages` declares that package (union of `kotlin_module` packages
and central-directory class packages). One jar sits in many nodes. Composition is a cheap union of the
per-jar `JarPackages`. This is a **merged view of the classpath** — which jar contributes a package vs
a class is invisible to resolution.

### Per (jar, package) members — lazy, cached
On first resolution of a name in package `p` of jar `J`:
- parse **only** `p`'s facade parts' `@Metadata` statics (the names `kotlin_module` gave for `p`) → functions;
- parse `p`'s `.kotlin_builtins` → builtin type names.
Cache `(J, p) → PkgMembers`. Sibling packages' facades are never parsed.

### Top-level memo — on the `Classpath`
`fqn → ResolvedEntry` (`Class | Function | Builtin | TypeAlias | Absent`). The single result cache; the
per-(jar,package) parses are intermediate.

## Resolution

### Qualified name `a.b.c.d` (FQN or explicit import) — depth, longest package prefix
Walk the tree **depth-first, consuming the longest package prefix** (kotlinc's behaviour):
1. `a` package? → node. `a.b` package? → node. `a.b.c` package? → *no* → stop; package = `a/b`.
2. Tail `c.d` = classifier `c` + nested `d`.
3. For `(package a/b, class c)`: search `node.jars` in **classpath declaration order**:
   - class: `find("a/b/c.class")` (O(1) zip); builtin: `a/b`'s builtins; first hit wins.
4. Nested `d`: resolve on the found class via its (lazy) `@Metadata`. (`Map.Entry` works this way.)
5. **Package-vs-class same name** (a segment that is both a subpackage and a class — rare, stdlib never
   does it): if the longest-package split fails, backtrack to a shorter package + class + nested.

Cross-jar split is a non-issue: `a.b` package (jar2) and class `a.b` (jar1) coexist because the tree
node aggregates jars and `find` searches all jars; longest-package-prefix picks the package split first.

### Simple name `X` (wildcard / default imports) — precedence levels, ambiguity, not first-wins
Levels, highest → lowest (kotlinc):
1. same-file / same-package decls + **explicit imports** (`import a.b.C`) — these are FQNs (resolve as above).
2. **explicit star imports** (`import a.b.*`).
3. **default star imports**: `kotlin.*`, `kotlin.annotation.*`, `kotlin.collections.*`, `kotlin.comparisons.*`,
   `kotlin.io.*`, `kotlin.ranges.*`, `kotlin.sequences.*`, `kotlin.text.*`, `kotlin.jvm.*`, `java.lang.*`.

For each level in order, resolve `X` in each of its packages (tree node → jars → find/facade/builtin).
**Stop at the first level with any hit.** Within that level: exactly one hit → resolve; **two or more
hits → ambiguity error** (matching kotlinc — `import java.util.*` + `import java.sql.*` then bare `Date`
is an error, *not* first-wins), unless the source qualifies the name. Explicit import (level 1) shadows
all lower levels.

### Functions (top-level / extension)
`package node → jars → that jar's kotlin_module facades for the package → parse those facade parts'
statics (lazy) → match by name (and receiver descriptor, for extensions)`. This replaces the eager
`ensure_ext_index` full-`*Kt`-facade scan.

## Eager vs lazy summary

- **Eager, once per jar:** shallow `kotlin_module` parse (~8 KB) + central-directory package-name pass.
- **Lazy per (jar, package):** facade statics + `.kotlin_builtins`.
- **Lazy per class:** `@Metadata` members.
- **Class find:** O(1) zip lookup, class bytes parsed only when members are needed.
- **jimage (JDK):** build package membership from its location table (names only), or skip it — JDK
  types reach the resolver via `jvm_class_map` mapping, not default-import probing. Decide in impl.

## Correctness invariants (must match kotlinc)

- Precedence: explicit / same-package > explicit-star > default-star; FQN exact.
- Depth: longest package prefix wins; tail = class + nested.
- Jar order: classpath declaration order within a resolved split; first hit for a class.
- **Same-level star multi-hit = ambiguity error** (not first-wins).
- Package/class same FQN: try longest-package split first, then backtrack.
- Typealiases still consulted (`type_alias_target`); nested classes via split.
- Correctness gate: **box conformance FAIL:0** (resolution byte-identical) at every step.

## Integration

- Reuse `EntryCache<T>` (already committed, `d8bbc91`).
- New: `JarPackages` + its shallow builder; the composed package tree (cached on `Classpath`); the
  per-(jar,package) `PkgMembers` cache; the `fqn → entry` memo.
- Route `resolve_type` / `SymbolSource::functions` through the tree instead of per-default-package probing.
- Retire the eager `ensure_ext_index` scan in favour of lazy per-package facade parsing.

## Rollout — each step gated on box FAIL:0 + a re-profile

0. **Re-profile** post-per-jar to pin the exact current top hot function (confirm probing vs
   ext-scan vs `resolve_type` build is the 65% target).
1. `JarPackages` + `EntryCache` + package-tree compose — built alongside, asserted to match, no
   behaviour change.
2. Lazy per-(jar,package) facade / builtin parse + `PkgMembers` cache.
3. Wire simple-name (wildcard/default) resolution through the tree; verify precedence + ambiguity.
4. Wire FQN / explicit-import resolution (longest-package split, backtrack) through the tree.
5. Retire the eager ext-index scan and the per-package `resolve_type` probing.
6. Measure the coverage gate on a **clean, uncontended** run — target 5–10 min.

## Risks

- Ambiguity semantics — verify against kotlinc (the two-star `Date` case) before relying on it.
- Precedence order — preserve the existing default-import list and its order (see `[[simple-name-overmatch]]`).
- jimage — decide location-table catalog vs skip.
- Measure the gate alone (concurrent builds/profiles inflate it ~3×).
