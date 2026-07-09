# Plan: perfect resolve

Goal: eliminate resolver-driven skips and miscompiles so symbol resolution goes through ONE seam
(`resolve_symbols`/`resolve_type` on the federated `SymbolSource`), with no `functions()`/`properties()`
legacy path and no lossy erasure at resolution time.

Status snapshot (branch `metadata-primary-generic-signatures`):
- Box conformance: **FAIL 0** (box-OK 2370, skipped 4981).
- e2e: 6 failing (down from 16). 5 of 6 are resolver-driven; 1 (`is_string_typeof`) is JS backend.

## Fixed already this session

- **Nullable value-class references keep `?`** (`resolve.rs::resolve_ty`). Reference nullability is otherwise
  dropped, but a value/inline class has a distinct boxed-vs-unboxed representation (like a primitive), so
  `Result<T>?` must stay nullable — else `res!!.getOrThrow()` never unboxes the boxed `kotlin.Result`.
  Decided through the single federated authority `SymbolResolver::is_value`. Fixed the 11-test suspend cluster.
- **Extension lambda-parameter receiver ranking** (`symbol_resolver.rs::ranked_extension_overloads_by_recv`).
  The legacy `functions()` provider labels every candidate with the QUERIED receiver, so `IntArray.any`'s
  `(Int)->Bool` block param tied with `CharArray.any`'s `(Char)->Bool` and `it` mis-typed as `Char`. Now
  ranked by the candidate's real `generic_sig.receiver` via `source_receiver_rank`. Fixed `operator_get_set`
  and unblocks primitive-array HOFs (`any`/`all`/`map`/`filter`/`sum`).

## Remaining problems, by root cause

### Root cause #1 — two resolution seams coexist
`functions()`/`properties()` (receiver-indexed, mislabels the declared receiver) still back several callers
alongside `resolve_symbols`/`receiver_extensions` (which carry the real receiver + `source_receiver_rank`).
Two lambda-typing paths can disagree; the receiver-mislabel is a whole bug class (patched one symptom).
- Symptoms: `named_args_..._extension_fn_reorder` (classpath ext named-arg reorder), `JoinToStringTrailingLambda`
  (compile fail), FQ `runBlocking` trailing-lambda result-type binding (2 e2e).
- Remaining `functions()` riders: `default_synthetic_callable` (symbol_resolver.rs:856), member-lambda typing.

### Root cause #2 — `Ty` erases reference nullability + generics (JVM-shaped)
- `unresolved member 'value' on 'kotlin/Any'` (**42 corpus**): value-class backing field / member reached
  through an erased `Any` receiver (inlineClasses fake-override, generic SAM).
- `type mismatch: inferred kotlin/Result vs kotlin/Result?` (**32**): the Result-nullability family, fixed for
  locals but not yet for inference / member returns / casts.
- `run_catching_get_or_else` CCE: `getOrElse` inline splice gets a BOXED `Result` where its `-impl` wants the
  raw underlying.

### Root cause #3 — generic type variables erase to `Any` too early
- `unresolved reference 'T'/'A'/'x'/'a'` (**~86**), `operator cannot be applied to 'Any'/'M'` (**51**),
  `cannot infer type` (**34**). `assignable.rs` already models `TyCtx` type-vars; the checker's operator/member
  resolution still erases to `Any` instead of resolving via the declared bound.

Out of scope for "perfect resolve": parser (~200), lowering-deep (152), coroutine lowering (108), JS backend.

## Phased plan

**Phase 1 — collapse to one seam (drop `functions()`/`properties()`).**
Migrate every remaining caller (`extension_lambda_param_types`, `extension_lambda_receivers`,
`default_synthetic_callable`, member-lambda typing) onto `receiver_extensions`/`resolve_symbols` +
`source_receiver_rank`, then delete the legacy methods. Cures root cause #1 at the source.
Closes: named-arg reorder, joinToString, FQ runBlocking.

**Phase 2 — value-class type fidelity.**
Extend `?`-preservation (done in `resolve_ty`) to inference, member returns, and casts; resolve value-class
members on their real type, never erased `Any`; feed the `getOrElse`/`getOrThrow` splice the raw underlying.
Closes: `value on Any` (42), Result type-mismatch (32), `run_catching_get_or_else`.

**Phase 3 — type variables carry bounds.**
Wire `assignable.rs` `TyCtx` type-vars into checker operator/member resolution so `M+Int`, `T.foo()`, and
inference resolve via the bound. Closes: unresolved `T/A` (~86), operator-on-`Any`/`M` (51), infer gaps (34).

**Phase 4 — residual concrete e2e** (callable-reference resolution, remaining FQ shapes).

Rough resolver-driven reclaim: ~250 corpus skips + 5 of 6 e2e fails.
