# Call resolver analysis & unification plan

Base: `origin/master` @ 7ac38e7. `src/call_resolver.rs` = 1908 lines.

Goal: collapse the resolver's many parallel entry points into **one overload-selection
function** `(receiver, args, retType?) -> result`, and fold the *kind decision*
(function / constructor / invoke-operator) into that layer instead of `resolve.rs`.

---

## 1. What call_resolver is today

`call_resolver.rs` is the **arg-DEPENDENT** layer over a `SymbolSource`. The source
(`functions(name, receiver?) -> FunctionSet`, `resolve_type(internal) -> LibraryType`) is a
pure metadata oracle: it returns every overload with raw signature + flags, does **no**
selection and **no** type-variable binding. call_resolver picks the overload for the actual
argument types and binds generic receiver/param/return types.

### 1a. Generic-signature binding engine (the genuinely-shared core)
- `unify_gsig` — bind type vars by unifying a `GSig` node against an actual `Ty`
  (arrays, `FunctionN` incl. suspend-SAM `Continuation` handling, `Class` positional args).
- `gsig_to_ty` / `gsig_tys` — realize `GSig` → `Ty` under bindings (unbound var → `Any`).
- `seeded_gsig_binds` / `bind_gsig_return` / `bind_ext_ret` — seed from explicit type-args,
  unify actuals, produce the return `Ty`.
- `function_input_types` — lambda param types out of a function-typed `GSig`.
- `infer_constructor_type_args` — `Pair(1,2)` → `Pair<Int,Int>`.

This layer is clean and should stay. Everything else is built on it.

### 1b. Overload selection — **THREE near-parallel copies** (the mess)
| kind | entry | helpers |
|---|---|---|
| top-level | `resolve_top_level_callable` | `resolve_top_level_default_callable`, `resolve_top_level_inline_only_callable`, `default_arg_mapping` |
| extension | `resolve_extension_callable` → `_exact` | `ranked_extension_overloads`, `bound_logical_params`, `bind_extension_callable`, `resolve_extension_default_callable` |
| member | `resolve_instance` / `resolve_instance_member` → `select_instance_info` | `best_member_overload` |

Each independently re-implements: receiver-rank grouping, exact→widened→subtype→descriptor
applicability passes, most-specific pick, generic-return binding, and `$default`/omitted-arg
handling. The differences are **calling-convention only** (member = `invokevirtual` with `this`;
extension = `invokestatic` with receiver as leading arg; top-level = no receiver), NOT selection
logic.

### 1c. Applicability predicates — also duplicated
`arg_fits` (free) · `CallResolver::arg_fits` · `arg_fits_or_subtype` · `arg_subtype_assignable`
· `fun_arg_matches` · `reference_subtype` · `is_classpath_subtype`. Overlapping "does arg fit
param" rules with subtly different subtype / value-class / function-arity handling.

### 1d. Constructor resolution — **separate path**, not through `functions()`
Reads `LibraryType.constructors: Vec<LibraryMember>` directly:
- `resolve_constructor` — plain `ctor(args)` + value-class-erased + descriptor-form + subtype +
  single-underlying value-class synthesis.
- `resolve_synthetic_constructor` + `SyntheticCtorCall` — `DefaultConstructorMarker` overloads
  (value-class param shape `(…, marker)` vs omitted-default shape `(…, int mask, marker)`).
- `synthetic_default_ctor`, `synthetic_default_member` — `$default` synthetics.

### 1e. Member / property / companion
`resolve_instance`, `resolve_instance_member` (+`ResolvedMember`), `resolve_property_member`,
`resolve_companion`.

### 1f. Pre-body lambda-shape queries
`top_level_lambda_param_types` / `_receivers` / `_materialized`, `extension_lambda_param_types` /
`_receivers`, `lambda_return_overload_param_types`, `resolve_lambda_return_overload`. These return
lambda param/receiver types *before* lambda bodies are typed, so the checker can type the body.

### 1g. Flag predicates
`toplevel_is_inline` / `_is_suspend` / `_has_must_inline`, `extension_is_inline` / `_is_suspend`.

---

## 2. Callers (who depends on this surface)

Only three files call in: `resolve.rs` (45 sites), `ir_lower.rs` (42), `jvm/jvm_libraries.rs` (1).
Hottest entry points: `resolve_instance_member` (26), `resolve_instance` (20),
`resolve_extension_inline_callable` (10), `resolve_top_level_callable` (6), `resolve_constructor` (5).

**Key architectural fact:** call_resolver is entered *after* `resolve.rs` has already decided
*which mechanism* applies. call_resolver never chooses fn-vs-ctor-vs-invoke.

---

## 3. The `Test(param)` problem — three mechanisms, one syntax

`Test(param)` (bare-name callee) has three candidate mechanisms in Kotlin:
1. **Constructor** of a classifier named `Test`.
2. **Function** named `Test` (top-level / imported / member).
3. **Invoke operator** — `Test` is a value (local/param/property) whose type has `operator fun
   invoke`, i.e. `Test.invoke(param)`.

Kotlin semantics: functions and constructors share **one namespace** — a class name at a call
site *is* a reference to its constructors, resolved together with same-named functions by one
overload resolution. Invoke-convention ("variable as function") is a **separate, lower-priority**
mechanism, gated on `Test` being a value in scope; scope proximity dominates (a closer-scope
value+invoke beats a farther-scope function).

Today this priority walk lives in `resolve.rs` (bare-name dispatch ~L8735+; `record_invoke`
L4449). call_resolver is oblivious.

### Empirical: fn vs ctor same-name (kotlinc 2.4.0)
Verified with the reference compiler — there is **no ctor-vs-function priority**; they form ONE
overload set:
- ctor `Test(Int)` + `fun Test(Int)` (same signature) → **declaration error**
  `conflicting overloads`, rejected before any call. kotlinc forbids a ctor and same-named fn
  sharing a signature.
- ctor `Test(Int)` + `fun Test(String)` (distinct) → legal; args disambiguate
  (`Test(5)`→ctor, `Test("hi")`→fn).
- `interface Test` (no ctor) + `fun Test(Int)` → legal; fn is the only applicable candidate → wins.
- both applicable, equal specificity → `OVERLOAD_RESOLUTION_AMBIGUITY` at the call site.

Consequence: no precedence code is needed between fn and ctor. Register ctors as `functions()`
overloads keyed by the **type name** (NOT `<init>` — `<init>` is only the emit/descriptor detail
on the resolved result) alongside same-named funcs, `kind = Constructor`, and run one
`select_overload`: distinct sigs are picked by args, the same-sig collision can't occur (rejected
at declaration), a no-applicable-ctor case (interface) lets the fn win, and an equal-specificity
tie is a genuine ambiguity error — not a silent choice.

### Empirical: invoke-operator vs fn/ctor precedence (kotlinc 2.4.0)
| scopes | winner | rule |
|---|---|---|
| inner `val`(invoke) vs outer `fun` | INVOKE | closer scope wins |
| same-scope `fun` vs `val`(invoke) | FUN | fn/ctor beats invoke at the same level |
| same-scope `fun(String)` n/a + `val`(invoke) | INVOKE | within a level, fall back to invoke when no fn applicable |
| outer `val`(invoke) vs inner `fun` | INNERFUN | closer scope wins |

kotlinc's **resolution tower**, a two-key ordering:
1. **Scope distance dominates** — walk innermost scope outward; stop at the first level yielding
   *any* applicable candidate.
2. **Within one level** — functions + constructors first (one overload set); only if none is
   applicable, a value's `operator invoke` at that level.

So the total order is `(scope-distance, then kind: fn/ctor before invoke, then most-specific args)`.

### Where each piece lives
- **`resolve.rs`** owns the *scope walk* (only it has the lexical env): innermost→outermost, per
  level gather the candidate set — funcs+ctors by name from `functions()`, plus any in-scope
  value's type for invoke — hand the level's set to `select`; first level that resolves wins.
- **`call_resolver::select`** owns the *within-a-set* decision (scope-free): fn/ctor selected by
  args first, invoke fallback, most-specific pick, generic binding. call_resolver must NOT try to
  globally choose invoke-vs-fn, because scope distance overrides kind and it can't see scope.

### Proposed encapsulation
- **Fold constructors into `functions()`** under callable-name `<init>`, keyed by the *type name*
  (`Test`), as a new `FnKind::Constructor` (owner = `Test`, name = `<init>`). Then a single
  `functions("Test", None)` query returns top-level `fun Test` overloads **and** `Test`'s
  constructors together — matching Kotlin's unified namespace — and `select_overload` picks across
  both in one pass. The `<init>` name is the emit/descriptor spelling, NOT the lookup key.
- **Invoke stays a receiver-dispatch step**, but move its selection into call_resolver too: it is
  just "resolve member/extension `invoke` on the value's type" — the same `select_overload` with
  `name = "invoke"`, receiver = the value's type. What must remain in `resolve.rs` is the *scope*
  decision "is `Test` a value in scope?" (needs the lexical environment call_resolver doesn't own).

So the clean split becomes:
- `resolve.rs`: lexical/scope resolution only — is the callee name a value in scope? a package
  path? Produces `(receiver?, name, args)`.
- `call_resolver`: given `(receiver?, name, args, type_args, retType?)`, select across the unified
  candidate set (functions ∪ constructors ∪ invoke-members) and bind generics.

---

## 0. THE GOAL (locked)

**Remove the checker/lowerer resolution redundancy. The checker resolves each call ONCE and
records a COMPLETE resolved model; `ir_lower` reads that model and emits it — it never calls
`call_resolver` again.**

Why this is the priority:
- The IR already carries fully-resolved targets (`Callee::External { owner, name, descriptor }`,
  `Callee::ExternalInstance {…}`, `New {…}`). The *shape is right*; only the *filler* is wrong —
  today the **lowerer** re-runs `call_resolver` to build those `Callee`s, after the **checker**
  already resolved the same call (to get its return type) and discarded the callable.
- Two costs eliminated: (1) double resolution per call; (2) the divergence hazard — checker and
  lowerer must feed byte-identical `arg_tys` or they pick different overloads → silent miscompile.
  The recurring "keep checker+lowerer arg-match guards symmetric" fixes are patches for exactly
  this hazard; a single resolution retires the whole class.
- Precedent already in the codebase: `ExprLowering::ExtensionPropertyGet` boxes a full
  `LibraryCallable` for the lowerer. Generalize that pattern to every call.

Non-goal for now: moving the scope walk (fn/ctor/invoke dispatch) out of the two passes. That
stays where it is. This task is strictly: **make the checker's handoff complete so the lowerer
stops re-resolving.**

## 4. Target: one selection function feeding a complete handoff

```
select(receiver: Option<Ty>, name, args, type_args, opts) -> Option<Resolved>
```

- `receiver = None`  → TopLevel funcs + Constructors named `name`.
- `receiver = Some(t)` → Members + Extensions on `t` (+ invoke when `name == "invoke"`).
- `opts` carries: `allow_must_inline` (inliner), and the caller's expected result shape.
- `Resolved` is a superset enum/struct spanning the current `LibraryCallable` /
  `LibraryMember` / `ResolvedMember` / `SyntheticCtorCall` outputs — carrying `kind`
  (Member/Extension/TopLevel/Constructor) so the caller emits the right convention, plus the bound
  return `Ty` and `suspend`/`default_call` flags.

The reference implementation already exists on branch `metadata-primary-generic-signatures`:
`select_overload(lib, recv, name, args, type_args, kind, allow_must_inline)` +
`logical_value_params` (strips the extension receiver so member/extension match identically) +
`best_by_args` (exact → Any-widened → prefix under-application → trailing-default-lambda) +
unified `arg_assignable` / `ref_subtype_fits`. That collapses member+extension. Remaining work to
reach "one function": fold in **top-level**, **constructors** (`<init>` in `functions()`), and
**invoke**, and unify the `$default` handling (currently 3 copies).

### retType? role
The optional expected return type feeds two things kotlinc uses:
1. `@OverloadResolutionByLambdaReturnType` (`sumOf { … }`) — pick the overload whose return equals
   the lambda's/expected type (today: `resolve_lambda_return_overload`).
2. Return-type-driven inference where args underdetermine type vars — seed binds from the expected
   type before realizing `gsig.ret`.

---

## 5. Duplication to delete (copypaste inventory)

- 3× receiver-rank grouping + applicability-pass ladder (top-level / ext / member).
- 3× `$default` handling (`resolve_top_level_default_callable`,
  `resolve_extension_default_callable`, the prefix pass in `best_member_overload` +
  `default_arg_mapping`).
- ~6 overlapping arg-fits predicates → 1 `arg_assignable` + 1 `best_by_args::fits`.
- 2× lambda-param-types / lambda-receivers (top-level vs extension) — same shape, differ only by
  whether `params[0]` is the receiver.
- `resolve_instance` vs `resolve_instance_member` — the former is the latter minus the
  bound-return; collapse to one returning `ResolvedMember`, drop the thin wrapper.

---

## 6. Constraints / gotchas (from memory + code comments)

- **No hardcoded method-name lists** — every method/return shape comes from `@Metadata`/source.
- Default-omitting passes must stay subtype/function-arity aware or `fold`/`map`/`joinToString`/
  property-ref regress (memory: resolver-unification GOTCHA).
- The prefix under-application pass must gate `required <= args.len()` or a 1-arg call binds a
  2-required value-class-mangled member and shadows the real extension (build.775 ee1).
- Value-class params: logical type (`Id`) vs erased underlying (`kotlin/String`) — logical must win
  in matching.
- Suspend `$default` carries `Continuation` before mask/marker (one slot longer).
- Constructors folded into `functions()` must NOT break the existing `LibraryType.constructors`
  consumers (value-class lowering, synthetic-marker ctors) — the ctor path in §1d has semantics the
  overload ladder doesn't (mask synthesis, `DefaultConstructorMarker`), so `<init>` overloads in
  `functions()` are for **selection/typing**; the synthetic-marker emit detail stays on the
  `resolve_type` path or is carried on the resolved result.

---

## 7. Phased plan (toward the §0 goal)

The end state: `ir_lower.rs` has **zero** `call_resolver::` calls; each call site's `Callee`/`New`
comes from a `ResolvedCall` the checker stored per `ExprId`.

1. **Define `ResolvedCall`** — the complete handoff, a superset spanning the current `Callee`
   variants + `New` + invoke forms, carrying: `kind` (Member/Extension/TopLevel/Constructor/Invoke),
   `owner`, `name`, `descriptor`, `bound_ret`, `default_call`, `suspend`, `vararg_elem`, and any
   arg coercions the lowerer currently recomputes. Add it to `TypeInfo` keyed by `ExprId` (mirror
   the existing `ExprLowering`/`expr_lowers` map).
2. **Collapse the selectors** into one internal `select_overload` (member+extension first — the
   branch design is the seed), then fold top-level, then constructors, then invoke-target selection.
   Unify the 3 `$default` copies and the ~6 arg-fits predicates. Its output is a `ResolvedCall`.
3. **Checker records once** — every call site in resolve.rs that resolves a call stores the
   `ResolvedCall` into `TypeInfo` (it already computes it for the return type; just stop discarding).
4. **Rewrite the lowerer to read, not resolve** — replace each of ir_lower's 42 `call_resolver::`
   sites with a `ResolvedCall → Callee/New` mapping. Delete the lowerer's `resolver()` helper.
5. **Delete dead wrappers** — the public entry points that only the lowerer used disappear;
   `call_resolver` shrinks to the binding engine (§1a) + one `select` + the ctor synthetics that
   carry genuine emit specifics.

Each phase is independently shippable and TDD-gated (`./run-tests.sh`). Order matters: 1→3 make the
handoff complete before 4 removes the lowerer's fallback, so the harness stays green throughout
(the lowerer can assert-or-fallback during migration, then drop the fallback in step 4).

### Migration safety
Keep a temporary invariant check: during steps 3–4, have the lowerer resolve AND compare against the
stored `ResolvedCall`; any mismatch is a pre-existing checker/lowerer divergence surfaced — fix it,
then remove the check. This converts the silent-miscompile class into loud failures during the port.
