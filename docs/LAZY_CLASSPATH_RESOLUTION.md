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
`fqn → ResolvedEntry`. The single result cache; the per-(jar,package) parses are intermediate.

`ResolvedEntry` is NOT an exclusive enum — Kotlin has **two namespaces** and one name can occupy both at
once, so the entry is a **record of coexisting namespace occupants**:

```
ResolvedEntry {
  classifier: Option<Classifier>,   // class | interface | object | typealias | builtin — at most one
  callables:  Callables,            // Functions(FunctionSet) | Property(PropertySet) | None
}
```

- **classifier** and **callables** live in SEPARATE namespaces, so `class test` may coexist with
  `fun test` or with `val test`.
- Within callables, `fun` and `val` of the same name are a **redeclaration error** (a name is functions
  XOR a property, never both) — kotlinc reports "conflicting declarations".
- `Absent` = both fields empty.

### Namespaces and call resolution
Resolution is **position-driven** — the syntactic position selects which namespaces of the entry apply:

| Position | Consumes |
|---|---|
| type (`x: test`) | `classifier` only |
| call (`test(args)`) | `callables.functions` ∪ the `classifier`'s **constructors** (direct candidates), then a function-typed `callables.property` invoked via `invoke` (strict FALLBACK) |
| value (`val y = test`) | `callables.property`; the `classifier` only when it is an `object`/companion instance |

**Constructors are not a separate resolver output** — a `classifier` used in call position contributes
its constructors as function-like candidates. So a CALL resolves against the union
`{ functions ∪ classifier-constructors }` by overload resolution; the variable-as-function (`invoke`)
candidate is admitted ONLY when no function/constructor is applicable. Two applicable function-like
candidates of equal specificity (`fun test()` and a no-arg `class test`) ⇒ overload-resolution ambiguity.

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

### Functions and properties (top-level / extension)
Top-level and extension **functions AND properties** resolve the same way, driven by the tree and the
import scope:
`for each in-scope package → tree node → jars → that jar's kotlin_module facades for the package → parse
those facade parts' statics (lazy, cached per (jar,package) as PkgMembers) → match by name (and receiver
descriptor, for extensions)`. A top-level property's static getter and an extension property's
receiver-leading getter come from the same facades. This replaces the eager `ensure_ext_index`
full-`*Kt`-facade scan.

**Import scope is REQUIRED here**, not optional: an unqualified top-level/extension callable is visible
only if its package is imported (same-package / explicit / star / default) — kotlinc. Scoping also makes
the lookup cheap: only the ~10 in-scope packages' facades are consulted, not every `*Kt` class. A
callable declared in the CURRENT module (not the classpath) is always visible and is never scope-filtered
(module visibility is resolved separately; its facade may be package-less).

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
- **Classifier is at most one** per resolved name. Two granularities decide it:
  - the SAME fqn (`a/b/Test`) present in several classpath entries → the FIRST entry in classpath
    declaration order wins (`find` searches `node.jars` in order, first hit; the duplicate dedups to one
    internal name).
  - the SAME simple name in DIFFERENT packages (distinct fqns) in scope at the same precedence level →
    ambiguity error, not first-wins; a higher level shadows a lower one.
- Correctness gate: **box conformance FAIL:0** (resolution byte-identical) at every step.

## Integration

- Reuse `EntryCache<T>` (already committed, `d8bbc91`).
- New: `JarPackages` + its shallow builder; the composed package tree (cached on `Classpath`); the
  per-(jar,package) `PkgMembers` cache; the `fqn → ResolvedEntry` namespace-record memo.
- Route `resolve_type` / `SymbolSource::functions` / `SymbolSource::properties` through the tree instead
  of per-default-package probing and the whole-classpath ext scan.
- The import scope threads into function/property resolution (a current-module callable is never
  scope-filtered; a classpath one needs its package imported).
- Retire the eager `ensure_ext_index` scan in favour of lazy per-package facade parsing.

## Rollout — each step gated on box FAIL:0 + a re-profile

0. **Re-profile** to pin the hot function. ✅ Found the racing `scan_types`/`type_alias_target`
   (`collect_signatures` ~67%), not the hypothesised ext-scan.
1. `JarPackages` + `EntryCache` + package-tree compose (incl. jimage packages). ✅
2. Lazy per-(jar,package) facade / builtin parse + `PkgMembers` cache. ◐ builtins + class `@Metadata`
   lazy; facades enumerated from `kotlin_module`; distinct `PkgMembers` cache lands with step 5.
3. Simple-name resolution through the tree; precedence LEVELS + same-level ambiguity — unified across the
   signature pass and the Checker (`resolve_name_against_imports`). Kotlin defaults outrank platform
   defaults so `Comparable`/`Number`/`CharSequence` (in both `kotlin.*` and `java.lang.*`) are not
   spuriously ambiguous. ✅
4. FQN / explicit-import (longest-package split via tree). ◐ `find` is tree-routed; explicit nested split
   still the `/`→`$` heuristic.
5. **Retire the eager ext scan; route function AND property resolution through the tree, scope-pruned;
   the `ResolvedEntry` namespace record + call-position selection (functions ∪ classifier-constructors,
   then property-`invoke` fallback).** ⬜ in progress — the current import-scoping is a post-filter over
   the ext index, not yet tree-driven; classifier/callable unification at the call site not yet done.
6. Measure the coverage gate on a **clean, uncontended** run — target 5–10 min.

## Risks

- Ambiguity semantics — verify against kotlinc (the two-star `Date` case) before relying on it.
- Precedence order — preserve the existing default-import list and its order (see `[[simple-name-overmatch]]`).
- jimage — decide location-table catalog vs skip.
- Measure the gate alone (concurrent builds/profiles inflate it ~3×).

## Symbol-resolve rewrite — status & handoff (in progress)

Goal: make `SymbolSource::resolve_symbols(fqn) -> ResolvedSymbols { classifier, callables }` THE query;
delete `functions`/`properties`. A name is always an FQN; the resolver forms candidate FQNs from the
import scope. Receiver-coupled work (value-class receivers, `@JvmName` element variants, MRO rank,
return binding) is SELECTION+emit, done by the CONSUMER — resolution is receiver-less by fqn.

DONE (green, full box = box()=OK 2381, FAIL:0):
- `ResolvedSymbols`/`Callables` types (libraries.rs); `SymbolSource::resolve_symbols(fqn)` (no receiver)
  on all 3 sources (JvmLibraries, ModuleSymbols, CompositeSource); TDD in classpath.rs `fq_tests`.
- `PkgMembers` (`global_jar_pkg_members`, `jar_pkg_members`) re-keyed to `@Metadata` SOURCE name
  (`kotlin_name`) — so `sum`→`sumOfInt` is found under source name; JVM name stays for emit only.
- `resolve_symbols(fqn)` discovers classifier + top-level + EXTENSION declarations (receiver-agnostic,
  carry `generic_sig`), source-keyed via the tree (`functions_in_scope`/`resolve_entry`).
- TOP-LEVEL call path (`resolve_top_level_callable`) consumes `resolve_symbols(fqn)`.

REMAINING (the consumer half) — `select_overload` (call_resolver.rs ~line 1720) still uses
`functions(name, Some(recv))`. Switching it to `resolve_symbols` MINIMALLY regresses 185 box files, ALL
in one group: iterator/range/unsigned/sequence-with-index (`iterator`, `downTo`, `until`, `contains`,
`withIndex`, uint). Root cause: agnostic decls lose the receiver-coupled computations `functions()` does:
  1. receiver_rank / MRO — decls are all rank 0; `functions()` ranks per receiver supertype rung. Compute
     in `select_overload` from recv's MRO vs each decl's declared receiver.
  2. `metadata_receivers_allow` (read-only vs mutable collection receiver) — decl must carry source
     receiver types; consumer subtype-checks recv.
  3. value-class receiver visibility (`Result.getOrThrow`: metadata-public+inline, bytecode-private →
     must-inline) — decl carries a flag; consumer checks recv is the value class.
  4. `@JvmName` element variant pick (`sumOfInt` vs `sumOfLong`) by recv element — via decl `generic_sig`.
  5. return nullability (`Map.get: V?`).
Plan: ENRICH decls in `resolve_symbols` (source receiver w/ element, ret nullability, vc flag) so the
binding is NEUTRAL; `select_overload` filters/ranks/binds from the enriched decl (`unify_gsig` is
pub(crate) in call_resolver). Gate EACH piece with the FULL box run (NOT `KRUSTY_NO_RUN` — it misses the
runtime ClassCast/VerifyError this regresses). Then properties same pattern; delete `functions`/`properties`.
