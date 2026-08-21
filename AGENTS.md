# Compiler implementation and review rules

These are repository invariants, not suggestions. Read this file before changing the parser,
resolver, checker, symbol providers, common lowering, or backends. Reviewers should reject a change
that violates one of them even when its focused regression test passes.

## Names and symbols

- Source spelling is lookup input, not symbol identity. The parser retains the spelling and source
  span of a reference. Resolution binds it once to a stable, qualified, interned symbol identity.
- After resolution, types, annotations, properties, classifiers, and selected callables travel as
  resolved identities. Later phases must not recover a symbol from its spelling.
- Resolve every annotation reference through the normal scope/import rules, including an
  unqualified source spelling, and store its qualified `TypeName` identity. Preserve the original
  spelling only for source rendering and diagnostics; semantic checks compare qualified identities,
  never annotation strings or simple names.
- A qualified expression is resolved left to right. The first segment is selected at scope-tower
  priority; a local/property root wins over a same-named package or classifier. Failure on a later
  segment never backtracks and reinterprets the root.
- Scope and `SymbolSource` expose the same candidate shape. Candidate collection may combine both;
  overload selection is a separate operation over that combined list.
- Do not add origin-specific resolution paths for local/module/classpath/Java/Kotlin declarations.
  Providers normalize declarations into the common semantic model at their boundary.
- Do not infer semantic facts from names. Arity, function/SAM shape, members, constructors,
  annotations, nullability, and inheritance come from declarations or metadata.
- Constructors are ordinary callable candidates with a constructor kind, not a parallel overload
  system. Primary and secondary constructors share candidate construction, argument mapping, and
  selection; declaration annotations contribute typed selection facts instead of spawning another
  candidate structure.

## Phase ownership

- The resolver/checker owns name binding, candidate construction, overload selection, argument
  mapping, and semantic types. An unresolved or ambiguous reference is a frontend error.
- Common lowering consumes checked identities and decisions. It must not call a resolver, search
  imports/scopes/classpaths, retry a lookup, reconstruct an overload, or silently fall back.
- Backends own representation: JVM owners/descriptors, boxing/storage, value-class representation,
  bridges, platform supertypes, and platform-specific emitted helpers. These facts must not affect
  source resolution unless Kotlin semantics explicitly expose them.
- Kotlin metadata is authoritative for Kotlin declarations. Complete missing metadata decoding
  instead of adding stdlib/class-name/member-name exceptions. Java/classfile facts are used only for
  declarations that do not have Kotlin metadata.
- Inheritance traversal is core behavior over the common class model. Providers return direct
  declarations and direct supertypes; they must not independently return inherited duplicates.

## Interned-name rule

- `TypeName::render()` is a boundary conversion. Production uses are acceptable only when producing
  diagnostics/traces, serializing an external format, or handing a name to a backend/classfile API
  that genuinely requires text.
- Never render for lookup, equality, prefix/suffix/segment inspection, map keys, or round-tripping
  back through `type_name()`. Add/use a `TypeName` or name-tree operation instead.
- Likewise, do not intern arbitrary property/callable spellings merely to use a type-name API.

## No fallback rule

- Do not make an invalid intermediate state work by trying another scope, source, spelling, owner,
  or lowering path. Preserve the frontend error with a clean, self-explaining diagnostic.
- Compatibility branches are not harmless: if the common path replaces one, delete the branch and
  its helper. Do not retain it behind `#[allow(dead_code)]`.

## Review-first smell scan

Review these before reading a diff linearly:

1. `render()` outside diagnostics, tracing, serialization, or backend emission.
2. Any `resolve_*`, import/classpath lookup, overload scan, or spelling-based dispatch in
   `ir_lower.rs` or a backend.
3. “try X, then Y”, origin checks, and local/module/classpath branches after candidate collection.
4. Hardcoded Kotlin library members or classifiers that metadata should provide.
5. Semantic decisions based on JVM descriptors, primitive wrappers, value-class storage, bridges,
   or platform supertypes.
6. Name-derived arity/function/SAM shape, string annotation comparison, and non-FQN annotation
   checks after resolution.
7. Duplicate constructor/call/property candidate structures or copied overload-selection loops.
8. New `#[allow(dead_code)]`, silent `Ty::Error` recovery, skipped diagnostics, or tests weakened to
   fit the implementation.
9. Diagnostic tests that use substring/prefix checks, search with `any`, sort output before
   comparison, or assert only rejection. Assert the complete emitted diagnostics and their count.
   Differential tests also assert exact file, line, column, message, and order.

Useful audit commands:

```text
rg -n '\.render\(\)' src
rg -n 'resolve_|get_class|classpath|fallback|or_else' src/ir_lower.rs
rg -n '#\[allow\(dead_code\)\]' src
rg --pcre2 -n -U '(front_end_diagnostics|compiler_diagnostics|krusty_(stderr|stdout|errors)|\bdiags?\b|\bdiagnostics\b)[\s\S]{0,240}(contains|starts_with|ends_with|\.(any|all|sort|sort_by|sort_by_key)\()' tests
```

Every hit requires semantic review; the commands are smell detectors, not blind bans.

## Where reviewers look first

Follow data ownership rather than starting at the changed test:

1. Name identity and segment operations: `src/name_tree.rs`, `src/names.rs`.
2. Scope-tower lookup and normalized candidate collection: `src/resolve/scope.rs`,
   `src/symbol_source.rs`, `src/symbol_resolver.rs`, `src/module_symbols.rs`.
3. Candidate union, applicability, overload selection, diagnostics, and recorded decisions:
   `src/resolve.rs`, then `src/assignable.rs` for type compatibility.
4. Kotlin metadata normalization: `src/jvm/metadata.rs` and `src/jvm/classpath.rs`. Check that Kotlin
   metadata becomes common semantic structures at this boundary rather than leaking JVM queries into
   resolution.
5. Lowering and emission: inspect `src/ir_lower.rs` for accidental lookup or semantic decisions,
   then the relevant file under `src/jvm/` only for physical representation.
6. Tests: require a repository-owned regression plus kotlinc comparison through the harness when
   behavior or diagnostics are in question. Do not accept a corpus pin as the only regression.

When a review finds the same architectural mistake twice, add the invariant and a targeted audit
command here. Do not bury recurring rules only in a review comment or session transcript.

## Validation

- Work test-first and compare diagnostics/behavior with kotlinc through the repository harness.
- Use `./run-tests.sh`; do not use release builds for ordinary validation.
- Do not weaken or replace an existing test to make a new path pass. Add distinct tests for
  semantically distinct source forms.
- Resolver work is complete only when the conformance survey has no resolver-related skips or
  regressions, including previously skipped cases.
- Keep one test process unless parallel execution is strictly necessary. The suite-wide timeout is
  120 seconds by default and must remain configurable for slow systems; timeout output must identify
  the active test/case well enough to diagnose loops.

See also `docs/ARCHITECTURE.md`, `docs/COMPILER_REVIEW.md`, and `docs/TEST_HARNESS.md`.
