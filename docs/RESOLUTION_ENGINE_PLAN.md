# Resolution engine — landing sequence

Design: `RESOLUTION_ENGINE.md`. This file is the PR-by-PR plan.

**Acceptance criterion for the whole arc: the duplicate typers are GONE.** Each PR states what it
deleted. A PR that adds engine surface while leaving the path it replaces alive has not finished its
step. The gate (`./run-tests.sh`, real exit code, "all test binaries passed") is green at every
landed step, and each PR stands on its own.

## Inventory to delete

Tracked here so "what is left" is answerable at any point in the arc.

Four separate retry-to-fixpoint machines approximate demand ordering today, not one. Each PR takes
one of them, so no PR is a pure addition and none is unreviewably large.

| Symbol | File | Deleted by |
| --- | --- | --- |
| `finish_top_level_computed_property_inference` | `src/resolve.rs` | PR 1 ✅ |
| `PendingMemberProperty` + `finish_member_property_inference` | `src/resolve.rs` | PR 2 ✅ |
| `preinfer_module_returns_to_fixpoint` (`for _pass in 0..8`) | `src/resolve.rs` | PR 3 |
| `preinfer_file_returns_to_fixpoint` | `src/resolve.rs` | PR 3 |
| `file_may_depend_on_preinfer_names`, `file_has_preinfer_candidates` | `src/resolve.rs` | PR 3 |
| `infer_lit_ty`, `infer_lit_ty_scoped`, `infer_lit_ty_scoped_on_demand`, `infer_lit_ty_p` | `src/resolve.rs` | PR 4 |
| `InferEnv`, `InferenceSource`, `infer_top_level_property_expr`, `infer_getter_ty` | `src/resolve.rs` | PR 4 |
| `top_level_lambda_shape`, `top_level_lambda_shape_in_scope` | `src/resolve.rs` | PR 5 |
| `module_member_lambda_shape`, `lambda_return_overload_param_types` | `src/resolve.rs` | PR 5 |
| `provider_member_lambda_expectations` | `src/resolve.rs` | PR 6 |
| `member_extension_lambda_param_types`, `extension_lambda_shape` | `src/resolve.rs` | PR 6 |
| `lambda_shape_for_overload` | `src/resolve.rs` | PR 6 |
| `bind_ext_ret`, `bind_ext_ret_tracking`, `bind_defaulted_ext_ret`, `bind_defaulted_ext_ret_slots` | `src/symbol_resolver.rs` | PR 6 |
| `merge_generic_bindings`, `merge_generic_bindings_from`, `complete_bottom_constraint_bindings` | `src/symbol_resolver.rs` | PR 6 |
| `unify_ty`, `unify_ty_from_symbols` as public API (becomes solver-internal) | `src/symbol_resolver.rs` | PR 6 |
| `is_java` / `has_metadata` / `ctor_params.is_none()` provenance proxies in core | `src/resolve.rs` | PR 7 |

## PR 1 — Demand-driven declaration typing (landed)

**Added** `src/type_engine.rs`: `DeclKey`, `ResolutionState { Computing, Resolved(Ty),
Declined(DeclineReason) }`, the computing stack, the loop set, `TypeEngine::resolve`, and the ONE
decline point.

**Replaced** the eager, file-argument-order typing of top-level property declarations: a declaration
the walk cannot type is recorded and resolved afterwards on demand, memoised.

**Deleted** `finish_top_level_computed_property_inference`.

**Decided** (measured against kotlinc 2.4.10, in `SPEC.md`): same-file source order restricts what an
EAGER initializer may read; a cross-file forward reference is legal; an expression getter is
executable and may read a later declaration.

**Tests** `tests/resolution_order_independence_e2e.rs` (all permutations, `javap` descriptors,
kotlinc differential), `tests/resolution_cycles_e2e.rs`, `src/type_engine.rs` unit tests.

## PR 2 — One queue for every deferred property (landed)

**Changed** class-body property inference to record and resolve through the engine, keyed by
`DeclKey::member`, in the SAME queue as top-level properties. Two queues cannot both be served: one
has to run first, so a member waiting on a module property and a module property waiting on a member
cannot both be answered. `DeferredProperty` carries a `DeferredKind::{TopLevel, Member}` and the
driver dispatches on it; a member's own class is consulted before the module in `demand`, matching
the shadowing the walk's scope already recorded.

**Deleted** `PendingMemberProperty` and `finish_member_property_inference` — the scope-refreshing
retry whose rounds are bounded by the pending count — and the top-level-before-member ordering PR 1
had introduced.

**Tests** `a_class_member_reading_another_file_types_the_same_in_either_order`,
`a_module_property_reading_a_class_member_types_the_same_in_either_order` (both with `javap`
descriptor assertions across every permutation), `a_cycle_between_a_class_member_and_a_module_property_declines`,
`a_cycle_between_two_class_members_declines`.

## Corpus byte sweep — base `2da5a640` vs `a9f8e6a3`

Two passes over all 7352 files of `target/cache/box-corpus/2.4.10/compiler/testData/codegen/box`,
each file compiled on its own, per-emitted-class byte md5. The base binary was built in its own
worktree (its `target/cache` symlinked to the provisioned one) rather than copied: dist discovery is
relative to the build path, and a copied binary compiles nothing at all.

| Outcome | Count |
| --- | --- |
| Files covered, both passes | 7352 / 7352 |
| Newly REJECTED (regressions) | **0** |
| Newly accepted | 5 |
| Classes whose bytes changed | 5, across 2 files — both pre-existing nondeterminism |

The five newly-accepted files are all accepted by kotlinc 2.4.10 as well:
`evaluate/annotationClassWithInner.kt`, `objects/initializationOrder.kt`,
`package/initializationOrder.kt`, `regressions/nullabilityForCommonCapturedSupertypes.kt` compile
cleanly, and `evaluate/intrinsicConst/kt53272.kt` compiles once its own
`// LANGUAGE: +IntrinsicConstEvaluation` directive is passed — krusty reads that directive out of the
source, kotlinc takes it on the command line.

The two files with changed bytes — `super/unqualifiedSuper.kt` and
`traits/interfaceWithNonAbstractFunIndirectGeneric.kt` — are NOT attributable to this work: the BASE
binary produces four distinct md5s for `unqualifiedSuperKt$box$1.class` across six runs of the same
input. Emission order is keyed by hash iteration order over the class signature's member maps, which
is a separate pre-existing defect.

A first version of this sweep reported a perfect clean result while having compiled nothing at all:
the binary path was never exported into the worker shell, so every row was `<rejected>`, and only
3281 of 7352 files produced a row. The script now asserts full coverage and exits non-zero on a gap,
because a sweep that silently covers half the corpus reads exactly like a clean one.

## Compile time — base `2da5a640` vs `a9f8e6a3`

735 box-corpus files, one compiler invocation each, 8-way parallel, the two binaries interleaved in
one session because cross-run drift on this machine is far larger than the effect being measured.

| Round | base | landed |
| --- | --- | --- |
| 1 | 101s | 103s |
| 2 | 106s | 105s |

Within noise.

### Peak memory

Measured with `/usr/bin/time -l`, three synthetic modules of 2400 top-level properties each.

| Workload | base `2da5a640` | landed |
| --- | --- | --- |
| 2400 trivially-typed properties — nothing defers, both compile | 38.4 MB / 0.75s | 37.8 MB / 0.75s |
| 735 real corpus files, per-file — both compile | 101s / 106s | 103s / 105s |
| 2400 CHAINED properties, every one deferred | rejects: 5920 errors, 0 classes | 121 MB / 22.5s |

On anything the base can compile, cost is unchanged — the memo is not paying for laziness on the
ordinary path. The third row has no baseline at all: the base cannot compile that module, which is
the order-dependence bug itself, so its 10s and 102 MB are the cost of producing 5920 errors.

That third row is still worth stating as an absolute: 2400 chained declarations cost 22.5s and
121 MB, and the curve over 400 / 800 / 1600 / 2400 properties (3.1s / 3.0s / 5.7s / 22.5s) is
superlinear at the tail. Each resolution rebuilds its own import scope and symbol resolver, which is
per declaration rather than per module. Copying the module's property table into every declaration's
value scope — quadratic in the number of declarations — was removed in favour of a lookup hook, worth
about 10%; the rest of the constant is that per-resolution setup and is the natural thing to attack
when the engine's `compute` becomes a real checker run.

## PR 3 — Inferred function returns on demand

**Changes** `preinfer_module_returns` to resolve each function's inferred return through the engine
when it is first asked for.

**Deletes** `preinfer_module_returns_to_fixpoint` (`for _pass in 0..8`),
`preinfer_file_returns_to_fixpoint`, `file_may_depend_on_preinfer_names`,
`file_has_preinfer_candidates`.

## PR 4 — One expression typer

**Changes** the engine's `compute` to run the REAL checker over the declaration's body and read the
resulting expression type, replacing the reduced expression grammar of the signature pre-pass. The
demand seam moves into the checker's name resolution.

**Deletes** `infer_lit_ty`, `infer_lit_ty_scoped`, `infer_lit_ty_scoped_on_demand`, `infer_lit_ty_p`,
`InferEnv`, `InferenceSource`, `infer_top_level_property_expr`, `infer_getter_ty`.

**Tests** the four SPEC fixtures (elvis, lambda-result variable, module extension result,
member/extension written arity) run unchanged against the single path.

## PR 5 — Expected type as an engine input

**Adds** `Expected { None, Type(Ty), ContextDependent }` threaded into expression resolution, and
lambda parameter shaping read off the expected function type at the argument position.

**Deletes** `top_level_lambda_shape`, `top_level_lambda_shape_in_scope`, `module_member_lambda_shape`,
`lambda_return_overload_param_types`.

## PR 6 — One candidate set, one solver

**Adds** candidate collection with member/extension/static/top-level as candidate properties, and a
constraint system solved once, reporting determinacy itself.

**Deletes** the remaining lambda channels and every per-channel binding helper listed above.

## PR 7 — Consumers and the provider boundary

**Changes** lowering's type queries and the LSP to ask the engine.

**Deletes** the provenance proxies in core.

## PR 8 — Corpus sweep, perf, SPEC

Whole-corpus byte sweep (two-pass md5, deltas justified against kotlinc), before/after compile-time
measurement interleaved in one process, and the SPEC entries for everything the arc decided.
