# SymbolSource refactor plan

## Goal

Shrink `SymbolSource` (src/symbol_source.rs) from 17 methods to its irreducible core:

```rust
pub trait SymbolSource {
    fn functions(&self, name: &str, receiver: Option<Ty>) -> FunctionSet;
    fn resolve_type(&self, internal: &str) -> Option<LibraryType>;
}
```

Everything else is either a **type-shape fact** (belongs inside `resolve_type`'s
`LibraryType`), a **platform fact** (belongs in `TargetRuntime`, src/libraries.rs:165),
or an **impl artifact** that should not exist as a trait responsibility at all.

## Why only two

Two orthogonal queries a declaration provider must answer:

- `functions(name, receiver)` — overloads of a call name (members / extensions / top-level).
- `resolve_type(internal)` — the shape of a fully-qualified type.

Name resolution in Kotlin is **scope + imports → fully-qualified name**, done by the
*resolver*, which already holds each file's imports. The resolver forms FQ candidates
(same package, explicit/wildcard/default imports) and probes `resolve_type(fq)`; existence
*is* the shape query. There is no "simple-name → internal" primitive in Kotlin — the
`class_names` reverse index is a krusty artifact (hence its ambiguity-pruning). So the
resolver never needs a source-side simple-name lookup.

## Classification of the current 17 methods

### Keep (2)
- `functions`
- `resolve_type`

### Fold into `resolve_type` / enriched `LibraryType`
Type-shape facts keyed by internal name. `LibraryType` already does this pattern
(`value_underlying`, src/libraries.rs:753).

| method | fold target |
|---|---|
| `sealed_subclasses` | new `LibraryType` field |
| `constructor_named_params` | enrich `constructors` (`LibraryMember` lacks source param names + per-param default flags, src/libraries.rs:59) |
| `value_class_ctor_has_default` | subsumed by the ctor default flags above |
| `is_enum_entry` | enum entries on `LibraryType` |
| `value_class_property_member` | `LibraryType.members` with the mangled getter recorded |
| `infer_constructor_type_args` | expose generic ctor sigs on `LibraryType`; do the unify in the resolver (it is arg-*dependent*, so it never belonged in an arg-independent metadata trait) |
| `value_underlying` | already just reads `resolve_type().value_underlying` for the `Obj` case — delete the wrapper; the non-`Obj` platform-builtin case → `TargetRuntime` (`scalar_value_repr` / `unsigned_integer_box_type` already exist) |
| type aliases (`seed.type_aliases`) | add `TypeKind::Alias` + `dest_type` to `LibraryType`; `resolve_type` returns a redirect, resolver follows the chain (the resolve.rs:903 fixpoint loop moves here) |

### Move to `TargetRuntime`
Platform class-names / descriptors. `TargetRuntime` doc (src/libraries.rs:162) already
states the boundary.

| method | note |
|---|---|
| `jvm_descriptor_form` | JVM erasure normalizer — literally a descriptor op |
| `canonical_names` (seed field) | duplicate of `jvm_descriptor_form`: precomputed `internal → canonical JVM internal` (`kotlin/collections/List → java/util/List`), read only in `obj_is_subtype` (resolve.rs:5608). Exists as a baked map only because the checker holds `SymbolTable`, not a platform handle |
| `property_reference_type` | platform runtime type; overlaps existing `property_reference_impl` |
| `class_literal_type` | "on this platform" |
| `platform_default_import_packages` | platform config |
| `physical_property_getter_name` | JavaBean `getX` = platform spelling |

### Delete outright (derivable / artifact)
| method | note |
|---|---|
| `function_like_arity` | plain-`Fun` case = `ty.fun_arity()` free fn; property-ref runtime case → `TargetRuntime` |
| `seed` / `seed_shared` | the whole bulk name-index dump; `class_names` becomes resolver-side FQ probing over `resolve_type`; `SharedSeed`/`LibrarySeed` types deleted; `Rc` caching stays *inside* the JVM source as an impl detail behind `resolve_type` |

## Consumers to rewrite

- **src/resolve.rs** — `build_symbol_table` (resolve.rs:865) destructures `seed_shared()`
  into `class_names` / `type_aliases` / `canonical_names`, builds `ClassNames`
  (resolve.rs:219, an `Rc`-base + user-overlay lookup), runs alias-chain expansion
  (resolve.rs:889–930) and import disambiguation. Rewrite: `ClassNames` no longer holds a
  bulk `Rc<HashMap>`; misses fall back to resolver FQ-candidate probing over
  `resolve_type`. User decls stay as the overlay. Alias chains resolve on demand via
  `resolve_type` alias redirects.
- **src/resolve.rs:5608** — `obj_is_subtype` reads `canonical_names`. Rewrite to call a
  normalization the checker is handed at `SymbolTable` build (platform `jvm_descriptor_form`
  captured as a closure, so the checker still does not reference `crate::jvm`).
- **src/ir_lower.rs:19297** — `libraries.seed_shared().0.get(name)` single-name lookup.
  Rewrite as resolver FQ probe / `resolve_type` (kills the tuple `.0` structural leak).
- **src/jvm/jvm_libraries.rs:997** — the JVM `SymbolSource` impl. Drop `seed`/`seed_shared`;
  keep the `Rc`-cached classpath index internally; enrich `LibraryType` in `resolve_type`;
  move platform methods to its `TargetRuntime` impl; the alias / canonical / value-class /
  enum-entry / sealed-subclass metadata that today populates seed maps now populates
  `LibraryType`.
- **src/module_symbols.rs:143** — the module (AST) `SymbolSource` impl. Same shape: drop
  `seed`; the module's declared types answer via `resolve_type`.
- **src/symbol_source.rs** — `CompositeSource` loses all the delegated platform/seed
  methods; keeps `functions` (concatenate) + `resolve_type` (first-wins). `LibrarySeed` /
  `SharedSeed` type defs deleted.
- **Tests** — src/symbol_source.rs tests, src/call_resolver.rs:1670, src/module_symbols.rs:454
  reference `seed`/`FakeSource`; update to the 2-method surface.

## Staging (safest first)

1. **Zero-risk deletions.** Remove `function_like_arity` (use `ty.fun_arity()`), remove
   `value_underlying` wrapper (callers read `resolve_type().value_underlying`), merge
   `seed_shared` into `seed` (single method) as a pure cleanup. No behavior change.
2. **Platform move.** Relocate `jvm_descriptor_form`, `property_reference_type`,
   `class_literal_type`, `platform_default_import_packages`, `physical_property_getter_name`
   to `TargetRuntime`. Retarget call sites (callers usually hold `CompilerPlatform` =
   both traits, so low friction). Fold `canonical_names` into the checker's captured
   normalize closure; delete the seed field.
3. **`LibraryType` enrichment.** Add fields/kinds for aliases, sealed subclasses, enum
   entries, ctor param-names + default flags, value-class property member, generic ctor
   sigs, self-`internal`. Move `constructor_named_params` / `value_class_ctor_has_default`
   / `is_enum_entry` / `value_class_property_member` / `infer_constructor_type_args` /
   `sealed_subclasses` into `resolve_type`. Update the `@Metadata` decoder in
   src/jvm/ that populates these.
4. **Kill `seed`.** Make `ClassNames` fall back to resolver FQ probing over `resolve_type`;
   move alias-chain expansion onto `resolve_type` redirects; rewrite ir_lower.rs:19297;
   add per-lookup ambiguity handling. Delete `seed`/`seed_shared`/`LibrarySeed`/`SharedSeed`.

Each stage lands green on `./run-tests.sh` (TDD; the differential harness vs kotlinc is the
correctness oracle). Stage 4 is the largest and carries the ambiguity-pruning risk — do it
last, behind the box gate.

## Endpoint

```rust
pub trait SymbolSource {
    fn functions(&self, name: &str, receiver: Option<Ty>) -> FunctionSet;
    fn resolve_type(&self, internal: &str) -> Option<LibraryType>;
    // + seed / seed_shared until Stage 4 removes them
}
```

Platform facts on `TargetRuntime`; type-shape facts inside `LibraryType`; name resolution
back where it belongs — in the resolver, over the file's imports.

## Progress log

- **Stage 2 — DONE.** Seven platform methods (`value_underlying`, `jvm_descriptor_form`,
  `function_like_arity`, `property_reference_type`, `class_literal_type`,
  `platform_default_import_packages`, `physical_property_getter_name`) moved to `TargetRuntime`.
  `CallResolver` + signature-phase helpers widened `&dyn SymbolSource` → `&dyn CompilerPlatform`.
  Gate green. `SymbolSource`: 17 → 10.
- **Stage 3 — DONE** (option B, full fold). Six type-shape methods folded into `resolve_type`'s
  `LibraryType`:
  - New `LibraryType` fields: `type_params`, `sealed_subclasses`, `enum_entries`,
    `value_ctor_has_default`, `ctor_named_params`, `value_class_properties`.
  - New `LibraryMember.generic_sig` (parsed ctor generic sig, platform-neutral).
  - New `LibraryType` methods: `is_enum_entry`, `constructor_named_params(min_arity)`,
    `value_class_property(name)`.
  - `infer_constructor_type_args` → free fn in `call_resolver` (uses `type_params` +
    ctor `generic_sig` + `unify_gsig`).
  - Perf: the standalone `metadata::class_*` helpers decoded fresh *every* call and were
    uncached, so populating these in `resolve_type` is neutral-to-faster. Dead
    `classpath::metadata_constructor_named_params` removed.
  - `SymbolSource`: 10 → 4 (`seed`, `seed_shared`, `functions`, `resolve_type`).
- **Stage 4a — DONE.** `canonical_names` removed from the seed. `obj_is_subtype` normalizes
  collection identity via `jvm_descriptor_form` (the platform erasure it already uses for
  codegen) instead of a baked internal→JVM map. `LibrarySeed`/`SharedSeed`/`SymbolTable` lose
  the `canonical_names` field; `SharedSeed` is now `(class_names, type_aliases)`. Behavior
  preserved (`canonical_names` was built by calling `to_jvm_internal`, exactly what
  `jvm_descriptor_form` does for a reference type). Gate green.

Landed endpoint: **`SymbolSource` = `{seed, seed_shared, functions, resolve_type}`** (4).

## Stage 4b/4c — kill the seed (scoped follow-up, NOT a fold)

Reducing to `{functions, resolve_type, type_aliases}` needs a resolver phase-ordering rewrite,
not a mechanical fold. Findings:

- The seed's `class_names` is already **default-import-package-scoped** (not a whole-classpath
  index) — the author narrowed it (`jvm_libraries::seed_shared`) and built the import machinery
  (`import_wildcards` + exhaustive `collect_file_type_names`, whose comment states it exists "so
  a default-import-only seed still resolves imported values") to replace it. The disambiguation
  block (`resolve.rs`, "Explicit imports disambiguate…") already reproduces the seed's
  `class_names` by probing default/wildcard packages via `resolve_type`, including the
  collection-forcing (`List` → `kotlin/collections/List` wins by package order).
- **Blocker: phase interdependency.** In `build_symbol_table` the order is seed → user classes →
  alias expansion (reads `class_names.get(target)`) → import block (populates `class_names`).
  Alias expansion runs *before* the block and depends on `class_names` already holding classpath
  names; the block in turn reads `class_names` for dotted resolution. Killing the seed requires
  reordering (block before alias expansion), threading alias targets into
  `collect_file_type_names`, and emptying the `ClassNames` base — then iterating against the box
  gate for the narrow user-`typealias`-to-classpath-type miscompile cases.
- `type_aliases` does **not** fold into per-name `resolve_type`: classpath aliases live in
  per-package `*TypeAliasesKt` facades and need a scan, so they become a dedicated
  `type_aliases()` method (Rc-cached), replacing `seed`/`seed_shared`.
- Perf: killing the pre-built map shifts work to per-file `resolve_type` probing (~N names ×
  ~10 default packages). `cp.find` caches negatives (cheap misses), but `resolve_type` rebuilds
  `LibraryType` per call — memoize `resolve_type` (Rc-cached per internal) before/with this step.

Endpoint after 4b/4c: **`SymbolSource` = `{functions, resolve_type, type_aliases}`** (3).
