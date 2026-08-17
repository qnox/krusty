# The resolution engine

The design of one type-resolution engine for krusty, replacing the several partial typers that
answer "what is the type of this?" today. This document is the design; `RESOLUTION_ENGINE_PLAN.md`
is the landing sequence. Semantics decided here are recorded in `SPEC.md` with tests, as always.

## 1. The problem: no engine, therefore several typers

krusty answers the type question in unrelated places, each a partial reimplementation with its own
scope model, its own generic binding and its own idea of when to decline.

| Where | What it types | How it is driven |
| --- | --- | --- |
| `infer_lit_ty` / `infer_lit_ty_scoped` / `infer_lit_ty_p` (`src/resolve.rs`) | a property initializer / expression body, producing the property's DECLARED type | one eager pass in FILE ARGUMENT ORDER inside signature collection |
| `preinfer_module_returns` | inferred function returns and member property types, using the real checker | a fixpoint loop, `for _pass in 0..8`, over whole files |
| `Checker::expr_*` | everything, correctly | per file, after the two passes above |
| `provider_member_lambda_expectations`, `member_extension_lambda_param_types`, `extension_lambda_shape`, `lambda_shape_for_overload`, `top_level_lambda_shape(_in_scope)`, `module_member_lambda_shape`, `lambda_return_overload_param_types` | a lambda argument's parameter types | consulted in a fixed priority order at each call site; first to answer wins |
| `bind_ext_ret`, `bind_defaulted_ext_ret`, `unify_ty`, `unify_ty_from_symbols`, `merge_generic_bindings`, `complete_bottom_constraint_bindings` | generic bindings for one channel | called per channel, per call site |

The consequences are structural, not incidental. Each was fixed individually and the tail of failures
on a real 47-module project did not shrink (whole-module errors 419 → 233 over a session of such
fixes).

- The pre-pass had no elvis arm, so `val HOST = System.getenv("APP_HOST") ?: DEFAULT` typed as
  nothing; because every read of an untyped property reports "unresolved reference", ONE property
  produced 13 errors across 4 files.
- The pre-pass could not bind a type variable through a lambda's RESULT, so
  `listOf(dto).map { Item(it.id) }` typed `List<Any>`.
- Two definitions of "what does an extension call return" — one for emission, one for reporting —
  drifted: a `vararg` extension bound nothing and the field descriptor came out
  `Ljava/lang/Object;` where kotlinc writes `Ljava/lang/String;`. ABI-breaking, and invisible to any
  box test, because the program still runs.
- Among the lambda channels the member channel answered first, so the extension channel never ran:
  `{ entry -> … }` against `Map.forEach` was shaped by the two-parameter Java `BiConsumer`.
- Order dependence. Verified on `2da5a640` with a fresh `target/gate/krusty`:

  ```
  A.kt: val base = listOf(1, 2, 3)
  B.kt: val derived = base.map { it + 1 }

  krusty A.kt B.kt   -> ok: emitted 2 class file(s)
  krusty B.kt A.kt   -> error: cannot infer the type of property 'derived'; add an explicit type
  kotlinc A.kt B.kt  -> ok        kotlinc B.kt A.kt -> ok   (identical bytes both ways)
  ```

The last item is the one that cannot be patched. A pass that types declarations in argument order
answers a question whose answer depends on the order it was asked in.

## 2. The model: how kotlinc is built

Verified against the shipped compiler we already vendor,
`target/cache/kotlinc/2.4.10/kotlinc/lib/kotlin-compiler.jar`, not against memory or blog posts.

**Declarations carry resolve phases.** `org.jetbrains.kotlin.fir.declarations.FirResolvePhase`:

```
RAW_FIR, IMPORTS, COMPILER_REQUIRED_ANNOTATIONS, COMPANION_GENERATION, SUPER_TYPES,
SEALED_CLASS_INHERITORS, TYPES, STATUS, EXPECT_ACTUAL_MATCHING, CONTRACTS,
IMPLICIT_TYPES_BODY_RESOLVE, CONSTANT_EVALUATION, ANNOTATION_ARGUMENTS, BODY_RESOLVE
```

**One entry point for a declaration's type.** `ReturnTypeCalculator` declares
`tryCalculateReturnTypeOrNull(FirCallableDeclaration): FirResolvedTypeRef`. Two implementations:
`ReturnTypeCalculatorForFullBodyResolve` (everything is resolved already — read it) and
`ReturnTypeCalculatorWithJump` (an implicitly-typed declaration — JUMP to it and run real body
resolution for it, then read it). There is no second, simplified typer. The type of an implicit
declaration is what ordinary body resolution says it is.

**Memoisation and cycle detection are one object.**
`ImplicitBodyResolveComputationSession` holds exactly:

```
HashMap<FirCallableSymbol<?>, ImplicitBodyResolveComputationStatus> implicitBodyResolveStatusMap
List<FirCallableSymbol<?>> computingSymbolsStack
Set<FirCallableSymbol<?>>  nonTrivialLoops
```

— a memo keyed by declaration, the stack of declarations currently being computed, and the loops
found. `ReturnTypeCalculatorWithJump.recursionInImplicitTypeRef(…)` turns a hit on the stack into an
error type, which surfaces as the diagnostic kotlinc actually prints:

```
$ kotlinc C.kt          # val a = b ; val b = a
C.kt:1:9: error: type checking has run into a recursive problem. …
C.kt:2:9: error: type checking has run into a recursive problem. …
```

**Expected types are an input, not a side channel.** `org.jetbrains.kotlin.fir.resolve.ResolutionMode`
is a sealed hierarchy — `ContextDependent`, `ContextIndependent`, `WithExpectedType`,
`ReceiverResolution`, `AssignmentLValue`, `Delegate`, `UpdateImplicitTypeRef`, `WithStatus`,
`ArrayLiteralPosition` — threaded into expression resolution. A lambda's parameter types are a
consequence of the expected type reaching it, not of a per-candidate lookup.

**Inference is a constraint system.** `Candidate`, `NewConstraintSystemImpl`, `Constraint`,
`ConstraintSystemCompletionMode`, `ConePostponedResolvedAtom`. Candidates contribute constraints; the
system is solved once; applicability is a property of the solved system.

We do not copy the phase list — krusty's declaration model is not FIR's. We copy the three
structural properties: **one engine**, **invoked on demand and memoised**, **expected types and
constraints as first-class inputs**.

## 3. What we build

### 3.1 Entry points

Two, and only two, questions:

```rust
/// The declared type of a declaration: a property's type, a function's return type, a
/// primary-constructor parameter's type. THE answer — the field descriptor, the getter
/// descriptor and `@Metadata` all read this one.
fn declared_type(&self, decl: DeclKey) -> Resolution<Ty>;

/// The type of an expression in a resolution context, under an expected type.
fn expression_type(&self, ctx: &ResolutionContext<'_>, e: ExprId, expected: Expected) -> Resolution<Ty>;
```

`declared_type` is implemented BY `expression_type`: it reconstructs the declaration's resolution
context and runs the real checker over its initializer/body. There is no reduced expression grammar
anywhere. That is the property that makes the seven typers deletable rather than merely wrapped.

### 3.2 The resolution context

`ResolutionContext` carries everything a resolution depends on, so that "same context, same
expression" is the memo key and nothing can leak in from the pass that happened to run first:

- the file and its import scope (imports are file-scoped; see the cross-file `typealias` entry in
  `SPEC.md`),
- the lexical scope chain and declared type parameters,
- the implicit-receiver tower (`this` rungs, including the dispatch rung and extension receivers),
- the expected type,
- the current type-argument bindings.

### 3.3 Laziness, memoisation, ordering

```rust
enum ResolutionState {
    NotStarted,
    Computing,                 // on the stack
    Resolved(Ty),
    Declined(DeclineReason),
}
```

The engine owns `RefCell<HashMap<DeclKey, ResolutionState>>` plus a `Vec<DeclKey>` computing stack.
Publication into `SymbolTable` happens at seam points; the memo itself is never the symbol table,
because a resolution running inside a checker holds `&SymbolTable`, not `&mut`.

Consequences we assert as tests:

- **Order independence.** A declaration is resolved when first asked for, so the argument order of
  files and the order of references cannot change an answer. This is the fix for the A.kt/B.kt case.
- **The fixpoint loop dies.** `preinfer_module_returns_to_fixpoint`'s `for _pass in 0..8` and its
  `file_may_depend_on_preinfer_names` scheduling exist only to approximate demand ordering. A memo
  computes each declaration once; there is nothing to iterate.

### 3.4 Cycles

A `DeclKey` found already `Computing` is a cycle. The engine records the loop, declines every
declaration on the cycle, and reports one diagnostic per declaration on it — matching kotlinc, which
reports at each implicit declaration in the loop, not once at the entry. Self-cycles (`val a = a`),
mutual (`val a = b; val b = a`) and longer loops are the same mechanism. Termination is structural:
`Computing` is set before recursion.

### 3.5 Expected types

`Expected` is an enum, mirroring the distinctions kotlinc makes and no more:

```rust
enum Expected {
    None,                 // no expectation (statement position)
    Type(Ty),             // a written or propagated type
    ContextDependent,     // postponed: the enclosing call will decide
}
```

Expected types propagate INTO expressions. A lambda argument's parameter types are then read off the
expected function type at that argument position, which is produced by candidate resolution. The
seven lambda channels become one code path with no priority order, so the `Map.forEach` case is
decided by the rule `SPEC.md` already records — an expectation whose parameter count cannot fit the
lambda as written is not an expectation for this call — applied once, to all candidates, instead of
by whichever channel ran first.

### 3.6 Candidates and constraints

One candidate collection per call site, containing every callable that could answer, with member /
extension / static / top-level as *properties* of a candidate, never as separate operations or
separate lookups. Each candidate produces constraints; the system is solved once.

Determinacy is reported BY the solver, not tested on the result:

- a variable bound to ITSELF is not bound;
- a variable reached from two disagreeing arguments is not determined;
- testing the RESULT for `Any` is wrong — an unbound variable erases to its own bound, which may be
  `CharSequence`.

These three traps are already paid for (SPEC entries on the `vararg` extension result and on lambda
result binding); the solver encodes them once instead of each channel re-deriving them.

### 3.7 The provider boundary

Symbol lookup goes through the provider boundary only. Core never branches on where a declaration
came from. `is_java`, `has_metadata` and `ctor_params.is_none()` used as provenance proxies are
defects under the standing project rule; the engine has no module/classpath/local or per-classifier
kind special cases. Where behaviour genuinely differs it is a declared capability of the provider
(how a call is EMITTED is origin-specific; what it RETURNS is not — `SPEC.md`, "A same-module
extension reports the RESULT of its call like any other origin").

### 3.8 One decline point

Exactly one place decides "cannot determine" and returns `Declined(reason)`.

> A wrong declared type is a miscompile that runs green. A decline is a recoverable diagnostic.

This invariant is why the `vararg` extension bug was ABI-breaking and box-invisible, and it survives
the rewrite as a single function with a single set of reasons, rather than as a `None` returned from
whichever of thirty helpers gave up.

## 4. Semantics the engine must satisfy

These are recorded in `SPEC.md` and double as the test list. The engine is not correct until each is
satisfied by the ONE path, with its existing test passing unchanged:

1. **`a ?: b` types a property initializer** — nullability of the LEFT side discharged only; a
   numeric mix DECLINES rather than reusing arithmetic promotion (kotlinc's LUB erases to `Object`);
   a platform right side keeps its flexible type; `?: throw` leaves the left side's type; `return`
   must NOT be typed. `tests/elvis_signature_inference_e2e.rs`.
2. **A callable's type variable can be bound by a LAMBDA argument's result** —
   `listOf(dto).map { Item(it.id) }` is `List<Item>`; a labelled call declines; a signature whose
   formals are not all bound keeps what the ordinary path inferred.
   `tests/lambda_result_type_variable_e2e.rs`.
3. **A same-module extension reports the RESULT of its call like any other origin** — and only when
   the receiver and arguments DETERMINED it. `tests/module_extension_signature_result_e2e.rs`.
4. **A member and an extension of the same name are chosen by the lambda's WRITTEN arity** — a
   destructuring parameter is ONE parameter; implicit `it` is exactly one.
   `tests/member_extension_lambda_arity_e2e.rs`.

## 5. Testing

The plan is as much the deliverable as the engine.

1. **Order independence.** Multi-file fixtures compiled in several argument permutations; assert
   identical success AND identical `javap` descriptors, including the A.kt/B.kt case.
2. **Cycles.** Self-, mutual- and three-way cycles terminate, decline, do not hang.
3. **Differential vs kotlinc 2.4.10** (`target/cache/kotlinc/2.4.10/kotlinc/bin/kotlinc`) asserting
   FIELD and GETTER DESCRIPTORS, not merely that both compile. A value assertion can pass through a
   branch where the right and the wrong typing agree; that already hid a real bug in a green test.
4. **Every existing test of the deleted typers ported and passing** —
   `tests/elvis_signature_inference_e2e.rs`, `tests/lambda_result_type_variable_e2e.rs`,
   `tests/module_extension_signature_result_e2e.rs`, `tests/member_extension_lambda_arity_e2e.rs`,
   and the property-inference tests in `tests/e2e.rs`.
5. **Whole-corpus byte sweep** over `target/cache/box-corpus/2.4.10/compiler/testData/codegen/box`,
   per-file class-byte md5, every delta justified against kotlinc. Two-pass method: record pass 1,
   revert in place, rebuild, pass 2, diff. NEVER copy the krusty binary out of `target/` — dist
   discovery is relative to its build path and a copy compiles nothing at all, which reads exactly
   like a catastrophic regression. In zsh split a file list with `for f in ${(f)"$(cat files.txt)"}`.
6. **Performance.** Lazy resolution risks re-entrancy and repeated work; memoisation must keep
   compile time comparable. Measured before and after on a large corpus, interleaved in ONE process
   (cross-run drift is ~80%).
7. **Green gate at every landed step** — `./run-tests.sh` with a real exit code and the
   "all test binaries passed" line, never piped to `| tail`, which masks both.
8. **Adversarial review before every commit.**

## 6. Non-goals

IR-backend refusals are out of scope: "this suspend-function shape is not yet supported by the IR
backend", "this construct is not yet supported", "inline splice failed". Those are unfinished
backend implementation on a separate track.
