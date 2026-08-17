# Resolution engine — landing sequence

Design: `RESOLUTION_ENGINE.md`. This file is the PR-by-PR plan.

**Acceptance criterion for the whole arc: the duplicate typers are GONE.** Each PR states what it
deleted. A PR that adds engine surface while leaving the path it replaces alive has not finished its
step. The gate (`./run-tests.sh`, real exit code, "all test binaries passed") is green at every
landed step, and each PR stands on its own.

## Inventory to delete

Tracked here so "what is left" is answerable at any point in the arc.

| Symbol | File | Deleted by |
| --- | --- | --- |
| `preinfer_module_returns_to_fixpoint` (`for _pass in 0..8`) | `src/resolve.rs` | PR 1 |
| `preinfer_file_returns_to_fixpoint` | `src/resolve.rs` | PR 1 |
| `file_may_depend_on_preinfer_names`, `file_has_preinfer_candidates` | `src/resolve.rs` | PR 1 |
| `infer_lit_ty`, `infer_lit_ty_scoped`, `infer_lit_ty_p` | `src/resolve.rs` | PR 2 |
| `InferEnv`, `InferenceSource`, `infer_top_level_property_expr` | `src/resolve.rs` | PR 2 |
| `PendingMemberProperty` + its post-walk retry | `src/resolve.rs` | PR 2 |
| `top_level_lambda_shape`, `top_level_lambda_shape_in_scope` | `src/resolve.rs` | PR 3 |
| `module_member_lambda_shape`, `lambda_return_overload_param_types` | `src/resolve.rs` | PR 3 |
| `provider_member_lambda_expectations` | `src/resolve.rs` | PR 4 |
| `member_extension_lambda_param_types`, `extension_lambda_shape` | `src/resolve.rs` | PR 4 |
| `lambda_shape_for_overload` | `src/resolve.rs` | PR 4 |
| `bind_ext_ret`, `bind_ext_ret_tracking`, `bind_defaulted_ext_ret`, `bind_defaulted_ext_ret_slots` | `src/symbol_resolver.rs` | PR 4 |
| `merge_generic_bindings`, `merge_generic_bindings_from`, `complete_bottom_constraint_bindings` | `src/symbol_resolver.rs` | PR 4 |
| `unify_ty`, `unify_ty_from_symbols` as public API (becomes solver-internal) | `src/symbol_resolver.rs` | PR 4 |
| `is_java` / `has_metadata` / `ctor_params.is_none()` provenance proxies in core | `src/resolve.rs` | PR 5 |

## PR 1 — Demand-driven declaration typing

**Adds** `src/type_engine.rs`: `DeclKey`, `ResolutionState { NotStarted, Computing, Resolved(Ty),
Declined(DeclineReason) }`, the computing stack, the memo, `TypeEngine::declared_type`, and the ONE
decline point.

**Replaces** the pre-inference fixpoint: each declaration needing an inferred type is resolved when
first asked for, memoised, instead of the file set being swept up to eight times until nothing
changes.

**Deletes** `preinfer_module_returns_to_fixpoint`, `preinfer_file_returns_to_fixpoint`,
`file_may_depend_on_preinfer_names`, `file_has_preinfer_candidates`.

**Tests**: `tests/resolution_cycles_e2e.rs` — self (`val a = a`), mutual (`val a = b; val b = a`),
three-way, and a cycle through a function return; each terminates, declines, and reports at every
declaration on the loop, matching kotlinc's per-declaration reporting.

## PR 2 — Signature collection asks the engine; the pre-pass typer dies

**Changes** `collect_signatures` to record implicitly-typed declarations as unresolved slots and ask
`TypeEngine::declared_type` for them, which runs the REAL checker over the initializer.

**Deletes** `infer_lit_ty`, `infer_lit_ty_scoped`, `infer_lit_ty_p`, `InferEnv`, `InferenceSource`,
`infer_top_level_property_expr`, `PendingMemberProperty` and its retry.

**Tests**: `tests/resolution_order_independence_e2e.rs` — multi-file fixtures compiled in every
argument permutation, asserting identical success and identical `javap` field and getter descriptors,
including the `A.kt`/`B.kt` case; the four SPEC fixtures (elvis, lambda-result variable, module
extension result, member/extension written arity) run unchanged against the single path.

## PR 3 — Expected type as an engine input

**Adds** `Expected { None, Type(Ty), ContextDependent }` threaded into expression resolution, and
lambda parameter shaping read off the expected function type at the argument position.

**Deletes** `top_level_lambda_shape`, `top_level_lambda_shape_in_scope`, `module_member_lambda_shape`,
`lambda_return_overload_param_types`.

## PR 4 — One candidate set, one solver

**Adds** candidate collection with member/extension/static/top-level as candidate properties, and a
constraint system solved once, reporting determinacy itself.

**Deletes** the remaining lambda channels and every per-channel binding helper listed above.

## PR 5 — Consumers and the provider boundary

**Changes** lowering's type queries and the LSP to ask the engine.

**Deletes** the provenance proxies in core.

## PR 6 — Corpus sweep, perf, SPEC

Whole-corpus byte sweep (two-pass md5, deltas justified against kotlinc), before/after compile-time
measurement interleaved in one process, and the SPEC entries for everything the arc decided.
