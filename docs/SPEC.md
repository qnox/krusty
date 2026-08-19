# krusty — a memory-lean Kotlin→JVM compiler PoC

**Status:** PoC / experiment. NOT a production Kotlin compiler.
**Goal:** demonstrate that a **linear, data-oriented, per-file streaming pipeline** compiles a
useful subset of Kotlin to JVM bytecode with a **working-set bounded by a single file**, instead of
the whole-module FIR+IR graph that makes `kotlinc` memory scale with module size.

This project is the concrete follow-up to the memory investigation in
`~/projects/kotlin-memory-bench` (see `COMPARISON_REPORT_2.4.0.md`): localized tuning of kotlinc
caps at ~8% on full compilation because the pipeline is whole-module; per-file processing measured
~80% lower peak. krusty *is* the per-file pipeline, built from scratch where there's no legacy
whole-module architecture or plugin contract to fight.

---

## 1. Design thesis

- **Linear pipeline, vertical execution.** Parse-all-signatures (cheap, global) → then per file:
  `typecheck body → lower → emit .class → drop`. At most one file's bodies/IR are live.
- **Data-oriented representation.** AST and IR are **structs-of-arrays indexed by `u32`**, not a
  pointer graph of boxed nodes. Spans, types, and symbols live in parallel arenas. This is the
  Zig/Carbon/rust-analyzer style — the opposite of kotlinc's `Fir*` object graph (~38M objects on
  a real build). Cache-friendly, header-free, bulk-freeable.
- **No GC, arena lifetimes.** Per-file arenas are dropped wholesale after the file is emitted.
- **Correctness by differential testing**, not by reimplementing kotlinc's exact output (§6).

## 2. Scope (what the PoC compiles)

### v0 supported Kotlin subset
- A single package; multiple `.kt` files compiled together.
- **Top-level functions**: `fun name(p: T, ...): R = expr` and block bodies `{ ... }`.
- **Types**: `Int`, `Long`, `Boolean`, `Double`, `String`, `Unit`. (No generics, no nullable types in v0.)
- **Expressions**: integer/double/boolean/string literals; arithmetic (`+ - * / %`), comparisons
  (`< <= > >= == !=`), boolean (`&& || !`), string `+` concat; parenthesization; calls to other
  top-level functions in the compilation; `if/else` as expression and statement.
- **Statements**: local `val`/`var` with inferred or explicit type; assignment; `return`; `while`.
- **Member calls** limited to a hardcoded JDK surface needed by tests (`Int.toString()`,
  `String` concat, `println`) — see §5.

### Explicit non-goals (v0)
Classes/objects/interfaces, generics, nullability & null-safety, lambdas/inline, extension
functions, properties with backing fields, `when`, smart casts, coroutines, multiplatform,
annotations/`@Metadata`, reflection, **all compiler plugins**, real Java-source parsing, incremental
compilation. Java *interop* in v0 = referencing a small fixed set of JDK class signatures (§5),
**not** compiling `.java`.

> Rationale: this subset covers the `kotlin-memory-bench` scenarios (`many_functions`, `multifile`,
> `bodyheavy`) — the exact workloads where the per-file pipeline showed ~80% lower peak — so krusty
> can be benchmarked head-to-head with kotlinc on identical inputs.

## 3. Pipeline (linear, per-file streaming)

```
                 ┌── global (cheap) ──┐      ┌──────── per file, streamed ────────┐
 source files →  lex → parse → collect →  for each file:  typecheck → lower → emit → DROP arena
                          (AST)   signatures                 (types)    (IR)   (.class)
```

- **Stage A — Lex** (`lexer`): byte slice → token stream. No allocation per token beyond a `Vec`.
- **Stage B — Parse** (`parser`): tokens → AST in an arena (`ast`). One arena per file; nodes are
  `u32`-indexed records in parallel `Vec`s.
- **Stage C — Collect signatures** (`resolve::sigs`): walk each file's top-level decls, record
  `(name, param types, return type)` into a **global symbol table**. Cheap; no bodies touched.
- **Stage D — Per file**:
  - **typecheck** (`resolve::check`): resolve names against the global table + locals, assign a
    `TypeId` to every expression, report diagnostics.
  - **lower** (`ir`): AST → a tiny stack-oriented IR (or straight to a bytecode builder).
  - **emit** (`codegen`): IR → JVM `.class` bytes via a hand-written class-file writer.
  - **drop**: the file's AST/IR/typecheck arenas are freed before the next file. ← the memory win.

Peak memory ≈ `global signature table` + `one file's AST+IR` + `fixed runtime`, i.e. ~constant in
file count, vs kotlinc's linear growth.

## 4. Crate layout

```
src/
  main.rs        # CLI driver: discover files, run the linear pipeline
  lexer.rs       # Stage A
  token.rs       # token kinds + spans
  ast.rs         # arena AST (SoA, u32 NodeId)
  parser.rs      # Stage B (recursive descent / Pratt for expressions)
  types.rs       # TypeId, primitive type table
  resolve.rs     # Stage C (signatures) + Stage D (typecheck)
  ir.rs          # tiny IR
  codegen/
    classfile.rs # JVM class-file writer (constant pool, methods, Code attr)
    emit.rs      # IR → bytecode
  diag.rs        # diagnostics (spans + messages)
  driver.rs      # orchestrates the streaming pipeline + arena drop points
harness/         # differential test harness (vs kotlinc) — see §6
tests/cases/     # .kt programs + expected behavior
docs/            # this spec + the implementation plan
```

## 5. Java / JDK interop (v0)

Real `.java` parsing and `.class` signature reading are deferred. v0 hardcodes a minimal
**builtin signature table** for the JDK symbols the test programs need:
- `java.lang.String` (concat, `length`), `java.lang.Integer.toString(int)`,
  `java.lang.System.out` + `java.io.PrintStream.println(...)`, `java.lang.Object`.
Kotlin `Int.toString()` etc. map to these via a small intrinsics table. Phase 5 (plan) replaces
this with a real `.class` reader (`cafebabe`/hand-rolled) so any JDK/Java dependency works, and
Phase 6 adds a minimal Java *source* front end for mixed compilation.

Integer literals participate in overload applicability using the candidate parameter types. An
exact `Int` parameter wins over adaptation to `Long`; non-literal `Int` values are not adapted.
Public static fields are valid class-qualified property reads, including in inferred property
initializers.

Public Java instance fields are valid value-qualified property reads. Selection uses the same
classifier/property hierarchy for Java source stubs, compiled dependencies, and source/module
subclasses: the nearest declaration wins, a private or static same-named field hides an inherited
instance field, and an actual field wins over synthetic JavaBean getter discovery. Generic field
types are substituted from the applied receiver; a raw receiver uses the descriptor-erased type.
Resolution carries the exact declaring owner, field name, and opaque physical descriptor through the
ordinary property-read node, including nullable safe-call lowering, rather than reclassifying the
receiver by origin during lowering or emission.

Static methods of **nested** Java classes resolve through all three kotlinc-accepted spellings —
`Outer.Bus.notify(x)` with `Outer` imported, `Bus.notify(x)` with `import pkg.Outer.Bus`, and the
fully-qualified `pkg.Outer.Bus.notify(x)`. A dotted qualifier chain maps to the JVM internal name
by resolving an in-scope outer class first (an in-scope type name shadows a package path, as in
kotlinc), then the trailing segments join with `$`; the fully-qualified form converts `/` → `$`
from the right until the type exists (`tests/java_nested_static_e2e.rs`).

## 6. Correctness & compatibility: differential testing vs kotlinc

**Compatibility IS a goal — specifically ABI + `@Metadata`, NOT byte-identity.** A krusty-compiled
`.class` must be usable as a drop-in library by Kotlin and Java consumers. That requires matching
the *contract* kotlinc produces, not the exact bytes:

- **Why not byte-identity:** kotlinc itself isn't byte-stable across versions (constant-pool order,
  `invokedynamic` vs `StringBuilder` concat, line tables, synthetic shapes). Byte-identity is
  unachievable *and* unnecessary — binary compatibility doesn't depend on it.
- **What IS required for library compatibility:**
  1. **ABI identity (exact).** Public class names + file→class mapping (top-level funs → `<File>Kt`),
     method/field **descriptors**, **modifiers/flags**, name mangling, `$default` methods for default
     args, `$annotations`/synthetic accessors. Consumers link against *this*; it must equal kotlinc.
  2. **`@kotlin.Metadata` equivalence (semantic).** A Kotlin consumer reads the protobuf-encoded
     `@Metadata`, not the raw signatures, to recover the Kotlin API (nullability, `val`/property vs
     method, default values, named params, variance). krusty must emit `@Metadata` that **decodes to
     the same Kotlin declarations** as kotlinc, with a compatible `metadataVersion`. (Semantic
     equivalence of the decoded protobuf — byte-identity of the annotation not required.)

Correctness/compat layers, strongest first (1–2 are the **primary gate** for library output):

1. **ABI diff (primary).** Parse both outputs' public members (names, descriptors, modifiers) and
   require an **exact** match. Any difference is a compatibility break.
2. **`@Metadata` diff (primary).** Decode `@kotlin.Metadata` from both (documented
   `kotlin-metadata-jvm` schema) and compare the recovered declarations; require semantic equality
   + compatible version.
3. **Execution differential.** Compile with both krusty and reference kotlinc (`kotlin-compiler`
   2.4.0 jar in `~/.m2`, headless); run a generated driver calling the functions with fixed inputs;
   compare results. Verifies behavior independent of code-gen shape.
4. **Structural disassembly (informational).** `javap -c -p` normalized; flags *how* code differs
   (e.g., concat strategy). Not a gate — shape may legitimately differ.
5. **Verifier (always).** Every `.class` must pass `java -Xverify:all`; non-verifying = fail.

The harness (`harness/`) is a Rust integration test shelling out to the reference compiler,
`javap`/a class-file parser, and `java`. Edge-case suite (§7) lives in `tests/cases/`.

## 7. Edge cases tracked (grow as implemented)

- **`suspend fun` (coroutines), slice 1 — the calling convention.** A `suspend fun` lowers to
  kotlinc's continuation-passing-style (CPS) JVM ABI: an extra `kotlin.coroutines.Continuation`
  parameter is appended and the return type erases to `java.lang.Object` (the resume value, *boxed* —
  a primitive return goes through a box, a reference return widens for free). A **leaf** suspend
  function (no suspension point in its body) needs no state machine: kotlinc emits exactly
  `public static final Object foo(Continuation)` with the boxed return, and so does krusty
  (`tests/suspend_e2e.rs::leaf_suspend_fun_has_cps_signature`; krusty boxes via `Integer.valueOf`
  where kotlinc uses `Boxing.boxInt` — runtime-identical; the generic `<? super …>` signature is
  erased). Architecture mirrors value classes: **ir_lower keeps the plain function and tags its
  `FunId` in `ir.suspend_funs`; the JVM-only pass `jvm::suspend::lower_suspend` owns the whole
  transform** (CPS signature now; the state machine + `Foo$fn$1` continuation class for functions
  with suspension points is a later slice). Until then, ir_lower's suspend gate skips (never
  miscompiles) any non-leaf shape: a suspension point, an extension/member suspend fn, or any *call*
  to a suspend fn (call-site continuation threading isn't modeled yet).
- **`suspend fun` slice 2 — the state machine.** A suspend function WITH a suspension point (a call to
  another suspend function) lowers to a coroutine state machine. `jvm::suspend` synthesizes a
  `Facade$fn$1 extends kotlin/coroutines/jvm/internal/ContinuationImpl` continuation class (fields
  `result: Object`, `label: int`, a `<init>(Continuation)` delegating to super, and `invokeSuspend`
  that stores the resume value, sets the `MIN_VALUE` label bit, and re-enters the function), and
  rewrites the body to: get-or-create its continuation (`$completion instanceof Facade$fn$1 && label &
  MIN_VALUE` ⇒ reuse, else `new`), read `result`/`COROUTINE_SUSPENDED`, then dispatch on `label` —
  state 0 calls the suspend callee with its own continuation and returns `COROUTINE_SUSPENDED` if the
  callee suspends, the resume state reads `result`; both yield the suspension value, bound once via a
  `when`-expression (a single store — assigning a pre-declared local in two branches trips the frame
  verifier). Built as ordinary IR (the emitter produces bytecode + frames), runtime-equivalent to
  kotlinc's `tableswitch` form (an `if`-chain dispatch). Proven end-to-end: a Java `Continuation`
  driver runs `bar` (`val a = foo(); return a + 1`) to completion → 43
  (`tests/suspend_e2e.rs::suspend_fun_with_suspension_point_runs_via_continuation`). Two supporting
  changes: `IrClass.field_private` (platform-neutral per-field visibility — the continuation's
  `result`/`label` are non-private so the facade reads them cross-class; the JVM emitter maps
  non-private → `ACC_PUBLIC`), and the constructor emitter now derives a *classpath* superclass's
  `super(args)` descriptor from the argument types. Still skipped (later slices): >1 suspension point
  (N states + local field spilling), suspension inside control flow, suspend lambdas / `suspend`
  function types, builders.
- **`suspend fun` slice 3 — N suspension points + local spilling.** A suspend function with multiple
  suspension points lowers to a `while(true){ val r = cont.result; <restore spilled>; when(label){…} }`
  dispatch loop: state 0 runs the prologue segment and calls the first suspend callee; each later state
  binds the previous result from `cont.result`, runs its segment, and calls the next callee; the final
  state runs the tail. A suspension-result local read in a later state is **spilled** to a synthesized
  continuation field (`L$0`, …) and restored at the loop top (its slot stays frame-consistent on every
  dispatch path). Two fixes this needed: the CPS continuation parameter's value-index collided with the
  body's first local (ir_lower numbers locals from the original param count) — `jvm::suspend` now shifts
  body locals up by one so the continuation owns that index; and `emit_cond_branch` folds a constant
  condition (`while(true)`) so the loop emits no spurious branch to method-end. Proven end-to-end:
  `baz` (`val a = foo(); val b = hundred(); return a + b`, `a` live across the second call) drives to
  142 (`tests/suspend_e2e.rs::suspend_fun_two_suspension_points_spills_live_local`). Still skipped:
  suspension inside control flow, suspend lambdas / `suspend` types, builders.
- **`suspend fun` — cross-unit suspend calls (resolver-driven detection).** A suspend call to a
  callee in ANOTHER compilation unit (a sibling source file, or a classpath dependency) has no
  `FunId` in *this* file's `suspend_funs`, so the same-file `suspend_set` can't see it. Detection is
  instead **resolution-time**: the `suspend` modifier flows uniformly into the resolver — from the AST
  (`Signature.is_suspend` → `module_symbols` → `FnFlags.suspend`) for a module/sibling fn, and from
  `@Metadata` (`IS_SUSPEND`, bit 13) for a classpath fn. ir_lower asks the resolver
  (`CallResolver::toplevel_is_suspend`, or the sibling `Signature.is_suspend`) and records each
  suspend call's `ExprId` → its *logical* return type in `ir.suspend_calls`. The coroutine pass treats
  any recorded `ExprId` as a suspension point (`is_suspend_call`) and threads the continuation; for the
  emitted call it derives the physical CPS shape — a `Callee::Static` descriptor gains the trailing
  `Continuation` param + `Object` return (`cps_descriptor`), a `Callee::CrossFile` gains the
  `Continuation` param type + `Object` return. The callee is *resolved by its logical signature* (no
  continuation, real return); the CPS form is the pass's job. The **classpath parser** enforces this:
  for a `suspend` top-level method (physical JVM form `Object foo(…, Continuation)`), `jvm_libraries`
  drops the trailing continuation parameter (`strip_continuation_param`) and recovers the logical
  return type from the selected metadata return class (e.g. `kotlin/Int` to `Int`), so a normal call
  resolves and types correctly; the erased `Object` return is kept only as `physical_ret`.
  Proven end-to-end both ways: `caller` (Use.kt) suspends on `helper` (Lib.kt, a separate `IrFile`),
  and against a **real** kotlinc-compiled `helper` on the `-cp` classpath, both reaching 43
  (`tests/suspend_e2e.rs::suspend_fun_calls_cross_file_suspend_fun`,
  `::suspend_fun_calls_classpath_suspend_fun`).
- **`suspend fun` — a call that is not spelled as a call is still a suspension point.** Kotlin's
  operator conventions desugar to calls, so `b[i]`, `b[i] = v`, `b += x`, `a < b`, `-a`, `!a`, `a..b`,
  `x in r` and `a++`/`a--` are suspension points whenever the operator they select is `suspend` —
  exactly like the call spelled out longhand. So is `a?.f()`, whose `Expr::SafeCall` is likewise not
  an `Expr::Call`. The coroutine CLASSIFICATION scan (`ir_lower::ast_body_suspends`, which decides
  whether a `suspend { … }` lambda gets a state machine at all; and `ast_execution_scope_suspends`,
  the file gate refusing a suspension in a non-suspend body) therefore cannot find them by call
  shape. It reads the checker's selected target instead, and must consult FOUR keys, because the
  checker files a convention target under whichever one fits the syntax:
  `resolved_calls[expr]` (a plain call, a safe call, indexed access `b[i]`),
  `resolved_operator_calls[(expr, op)]` (arithmetic, comparison, unary, `a..b`, `x in r`, and the
  value-returning `b += x` — which desugars to `b = b.plus(x)`, an EXPRESSION),
  `resolved_stmt_operator_calls[(stmt, op)]` (statement-position `a++`/`a--`, and an index STORE
  `b[i] = v`, both of which are statements with no expression to key), and
  `CompoundAssignmentTarget` (the in-place `b += x` selecting a `Unit`-returning `plusAssign`,
  recorded against the statement — the specialized emission target retains the selected callable
  capabilities). Relational syntax records its selected `compareTo` under the same operator table for
  source/classpath members and extensions; neither classification nor lowering reselects it by class
  name or symbol origin.
  Missing any of these misclassifies the ENCLOSING lambda as non-suspend, which is the dangerous
  direction: the callee still gets its CPS signature while the call site keeps the pre-CPS
  descriptor, and the resulting `NoSuchMethodError` is swallowed by the driving `Continuation` —
  `box()` returns a wrong answer instead of failing. (Before this was fixed the files happened to die
  at emit with no labelled reason, which is a refusal, but an accidental and unattributable one.)
  A convention suspension now behaves exactly like its longhand form, including where the
  state-machine pass still declines one: on a safe call's short-circuiting branch, or in the
  CONDITION of an `if`/`when` used as an EXPRESSION whose value is stored into a CAPTURED variable,
  it reaches the same labelled `SkipReason::Suspend` either way. That second boundary needs BOTH
  halves, which is why neither alone names it: the same suspending condition compiles when the
  `if` is a STATEMENT (`if (less()) { r = 7 } else { r = 9 }`), when the expression's value lands in
  a LOCAL (`val x = if (less()) 7 else 9; r = x`), and when it is returned from a suspend FUNCTION
  rather than assigned inside a suspend LAMBDA (`suspend fun drive(): Int = if (less()) 7 else 9`,
  and the `&&` shape in the short-circuit entry below).
  A `suspend` EXTENSION operator is NOT a separate restriction. `gate:extension-suspend-fn` is
  retired (see the two `suspend fun` extension entries below); the declaring file compiles, and a
  sibling-file convention call links against the real CPS entry point exactly like a same-file one.
  Every convention runs both same-file and cross-file — indexed read `b[i]` and store `b[i] = v`,
  binary `b + i`, in-place `b += i`, unary `-b`, relational `a < b`, and `x in b`.
  The one shape that still declines is the `if`/`when`-EXPRESSION boundary above, and it is about
  neither extensions nor conventions: `r = if (a < b) 7 else 9` inside a `suspend { … }` reaches
  `SkipReason::Suspend`, and so does the identical `r = if (less()) 7 else 9` written with a plain
  `suspend fun` call and no operator at all — same-file as well as cross-file. The convention is
  incidental. kotlinc compiles and runs every shape named here, refused ones included; for the two
  refused above it answers `7`
  (`tests/coroutine_intrinsics_e2e.rs::suspend_operator_get_convention_is_a_suspension_point`,
  `::suspend_operator_plus_convention_is_a_suspension_point`,
  `::suspend_operator_plus_assign_convention_is_a_suspension_point`,
  `::suspend_operator_compare_to_convention_is_a_suspension_point`,
  `::suspend_call_behind_a_safe_call_is_seen_as_a_suspension`,
  `::suspend_in_an_if_expression_into_a_captured_var_skips_without_a_convention` and its two
  disambiguating controls `::suspend_in_an_if_statement_condition_into_a_captured_var_runs`,
  `::suspend_in_an_if_expression_into_a_local_runs`;
  `tests/cross_file_inline_call_e2e.rs::suspend_operator_get_convention_cross_file_executes`,
  `::suspend_operator_plus_assign_convention_cross_file_executes`,
  `::suspend_operator_compare_to_convention_cross_file_runs_outside_an_if_condition`,
  `::suspend_operator_compare_to_convention_cross_file_still_skips_in_an_if_condition`;
  and in `tests/suspend_operator_convention_cross_file_e2e.rs` the five
  `::suspend_*_cross_file_executes` guards plus `::compare_to_and_contains_cross_file_execute`).
- **`suspend fun` — async resume + parameters live across a suspension.** Two correctness items the
  synchronous-completion tests couldn't reach. (1) The suspend-call sequence emits
  `when(result == COROUTINE_SUSPENDED) { return result }` before storing the synchronous value; its
  branch body must be a `Block` (the When-statement emitter drops a bare `Return`), else
  `COROUTINE_SUSPENDED` falls through to the unbox — a `ClassCastException` the instant a callee
  actually suspends. (2) A value PARAMETER read across a suspension must survive an async re-entry. It
  is spilled like a local, but — being live on ENTRY — the continuation also CAPTURES it at
  construction (`new Fn$1([this,] params…, completion)`), so the loop-top restore reads a correct value
  on the first iteration; the restore assigns the existing param slot (`SetValue`, not a fresh
  `Variable`, which would strand the param slot as `top`). `invokeSuspend` re-enters with type-correct
  placeholders for the params (kotlinc passes `iconst_0`), the real values coming from the captured
  fields. This unblocks member suspend fns WITH parameters (previously skipped). Proven by a real
  kotlinc `suspendCoroutineUninterceptedOrReturn` primitive that parks its continuation: a
  top-level/member suspend fn propagates `COROUTINE_SUSPENDED`, and a later `resumeWith` re-enters the
  state machine and delivers the result with the parameter intact
  (`tests/suspend_e2e.rs::suspend_fun_actually_suspends_and_resumes_async`,
  `::member_suspend_fun_with_param_survives_async_resume`,
  `::toplevel_suspend_fun_with_param_survives_async_resume`).
- **`suspend fun` — suspension on an elvis / safe-call RHS.** `x ?: foo()` lowers to a block-valued
  initializer `Variable { init: Block { prelude…, value: When } }` (the `When` selects the non-null
  value or the suspending `foo()`). `normalize_block_inits` rewrites that to `prelude…; Variable { init:
  When }`, surfacing the conditional suspension as the `Variable{init: When}` the flattener's
  `stmt_cond_suspension` already handles. Proven both branches: `bar(null)` suspends on the elvis RHS
  (→8), `bar(5)` takes the value branch with no suspension (→6)
  (`tests/suspend_e2e.rs::suspend_fun_suspension_on_elvis_rhs`).
- **`suspend fun` — suspension in an `if`/`when` CONDITION (`if (c && check())`).** A condition is
  evaluated unconditionally before its branch, so a suspension there is hoisted to a preceding bound
  temp — `hoist_stmt` now applies `hoist_expr` to a `When`-statement's branch CONDITIONS (the bodies
  stay for `emit_when_stmt`). Previously the condition's suspend call was left un-threaded
  (`invokestatic check(Continuation)` with no continuation argument → an operand-stack VerifyError).
  Proven: `if (c && check()) return 1` drives `bar(true)`→1, `bar(false)`→2
  (`tests/suspend_e2e.rs::suspend_fun_suspension_in_and_condition`).
- **`suspend fun` — an unnamed TEMP that is live across a suspension gets a spill slot.** Hoisting a
  multi-suspension expression (`a() + b() + c()`) materializes one unnamed `Variable` per operand, so
  the first operand's value must survive the *later* suspensions. The per-suspension scope snapshot
  (`ScopeWalk`, kotlinc's positional-spill model — see `docs/POSITIONAL_SPILLS.md`) previously admitted
  only `named: true` variables, so those temps were in the spilled union (hence stored) but in no
  resume arm's restore list (hence read back as `null`/a wrongly-typed slot). A NAMED variable still
  spills by lexical SCOPE (kotlinc's rule — every splice-materialization local is emitted `named` at its
  lowering site, so scope and liveness agree for them); an unnamed TEMP now spills by LIVENESS: it is
  included exactly when some expression that may still execute — a later statement of an enclosing list,
  or a whole enclosing loop, which re-runs on the back-edge — reads it. Liveness rather than scope is
  what keeps the per-kind field maxima at kotlinc's count: a dead temp would inflate them. kotlinc
  spills a live operand the same way (`a() + b()` stores its partial `StringBuilder` in `L$0` and
  restores it in every later arm). Proven: `runBlocking { pick(0) + pick(1) + pick(2) }` → `"abc"`
  (`tests/feature_coverage_j_e2e.rs::suspend_when_branch_around_suspend_calls`). The model covers a
  RECEIVER lambda too — its leading `this`/capture fields do NOT displace a temp's positional slot
  (`tests/suspend_e2e.rs::suspend_receiver_lambda_spills_hoisted_temps`). A spilled local of type
  `Nothing` still bails: its expression never yields a value, so the slot has no JVM type and merges to
  `top` at a join ("Bad local variable type" — `spills_bottom_typed_local`).
- **`suspend fun` — a `suspend` EXTENSION called through an explicit receiver is a suspension point.**
  `ast_body_suspends` classified the CALLER's body as leaf for `Ctl(40).run2()`: a top-level suspend
  extension reached that way is a `Member` callee, invisible to the bare-`Name` scan, and it is not an
  instance member of the receiver's type, so the resolved-member scan misses it too. The lambda then got
  no state machine and the call emitted without a `Continuation` ("call arity mismatch").
  The shape-free `collect_call_sites` scan closes it — every call is inspected through the CHECKER's
  selected target, so receiver syntax needs no name-matching heuristic of its own (the earlier
  `collect_member_call_names` name scan it replaced was narrower and keyed to the AST shape). An
  extension body may now suspend on a MEMBER
  of its receiver — the receiver is an ordinary parameter and the member call threads its own
  continuation, so `gate:extension-suspend-fn-member-suspension` is retired
  (`tests/suspend_e2e.rs::suspend_extension_suspending_on_a_receiver_member`). One residual shape the
  corpus proved is NOT about extensions keeps its own bail: a `try`/`catch` over a REAL suspension
  (`gate:suspend-try-catch`, below).
- **`suspend fun` — a suspend LAMBDA into a MEMBER function's `suspend`-function-typed parameter is
  ordinary, not a blocker.** `gate:suspend-lambda-into-member-parameter` claimed that
  `Controller.drive(c: suspend Controller.() -> Unit)` left its lambda argument a plain `FunctionN`
  whose body never threads a `Continuation`. It does not. A member call's parameter types come from the
  IR signature (`self.ir.functions[mfid].params`) and `ty_to_ir` is the IDENTITY on `Ty::Fun`, so
  `suspend` survives into `lower_arg`'s `Ty::Fun(s) if s.suspend` route exactly as it does for a
  top-level builder — the member and top-level paths never diverged. Verified end to end:
  `Holder().accept { val a = step(); a + "!" }` on a member `accept(block: suspend () -> String)` builds
  a real `SuspendLambda` (`box$suspend$0`), suspends and resumes to `"s!"`, matching kotlinc
  (`tests/suspend_e2e.rs::suspend_lambda_into_member_parameter_runs`). The gate was a pure
  false-positive file skip and is retired. Two things it was blamed for are separate and NOT
  member-specific: a suspend RECEIVER lambda that both suspends and calls a member of its receiver fails
  to verify identically through a top-level builder, and what actually blocks the corpus case the gate
  was attached to (`coroutines/suspendFunctionAsCoroutine/handleException`) is the `try`/`catch` entry
  below — a file with no member `suspend`-typed parameter at all reproduces that miscompile with the
  retired gate still enabled, so its scan was not merely too narrow but keyed to the wrong construct.
- **`suspend fun` — a `try` that CATCHES over a REAL suspension, with a value live across it, is
  refused rather than miscompiled.** The coroutine pass flattens a suspend body into a `label`-dispatch
  loop and wraps the whole loop in ONE `catch Throwable` (`jvm::suspend::wrap_dispatch_for_handlers`).
  That handler routes purely on which `label` was in flight: it stores the exception into `result`, sets
  `label` to the handler's state and re-enters the loop — WITHOUT restoring the locals the predecessor
  state spilled into the continuation. A resumed machine re-enters the static body as `f(null, …)`, so a
  parameter, extension receiver or spilled local read at or after the `catch` reads back `null`:
  `suspend fun f(): String { val a = ok("A"); try { boom() } catch (e: Exception) {}; return a + "-end" }`
  threw an NPE where kotlinc answers `"A-end"`. Not extension- or member-specific — that reproduction
  has neither a receiver nor a parameter, only an ordinary spilled LOCAL.

  `gate:suspend-try-catch` therefore keys on "a value is live across the `try`", and four conditions
  keep it off shapes that demonstrably round-trip today. (1) The `try` must have a CATCH: a
  `finally`-only handler always re-throws and never re-enters the loop
  (`tests/suspend_try_finally_body_e2e.rs`). (2) Its protected region must contain a REAL suspension —
  one whose callee chain reaches a suspension INTRINSIC
  (`suspendCoroutineUninterceptedOrReturn`/`suspendCoroutine`/`suspendCancellableCoroutine`), computed
  as a least fixpoint over the file's suspend declarations. Merely CALLING a suspend function is a
  suspension *point*, but a leaf chain returns synchronously, the frame is never re-entered, and the
  missing restore cannot be observed — which is why `suspend_in_catch_body_spills_exception` (whose
  `tick`/`setup` only append and return) keeps passing. (3) Some value must be live to lose: the owning
  suspend function has a value parameter, an extension receiver, or declares a local — so
  `suspend fun f(): Int { try { return d() } catch (e: Exception) { return d() } }` stays ACCEPTED
  (`backend_rejection_coverage_e2e::suspend_try_catch_accepted`). (4) The loss must be observable past
  the `try`: a catch body reads one of those names, or itself suspends (resuming INSIDE the handler
  needs the same restores), or the `try` sits in STATEMENT position so ordinary code can follow it.
  The scan walks each suspend body's reachable expressions, so a `try` inside a lambda, a local fun or a
  hoisted nested class is seen; "suspension point" is the same file-local name approximation
  `gate:suspend-call-from-non-suspend` makes, so a suspend callee from another file is not counted.

  The handler now DOES test the DECLARED CATCH TYPE (previously an accepted unsoundness): each catch
  arm in the handler state is guarded by `instanceof` on the stashed exception and a non-matching
  exception RE-THROWS, so `catch (e: Miss)` no longer swallows an `IllegalStateException` Kotlin
  requires to propagate to the completion — for a NON-suspending catch body (which previously ran
  unguarded) and a suspending one (which previously threw a `ClassCastException` at its exception
  bind) alike. A `Throwable`-typed catch is full-coverage: no guard, later arms dead, exact prior
  shape. The same handler generalizes to MULTIPLE catches when no catch body suspends (each arm
  emits inside the one handler state); a suspending catch body is still modeled only alone. And a
  VALUE-position `try` under a RESULT COERCION (`suspend fun f(): Base = try { sub() } catch …`, the
  cast the checker inserts to the declared return type) desugars like the bare form — the coercion
  moves onto each selected branch, `desugar_value_when`-style, and the desugar also rewrites the
  locally-BOUND form (`val v = try { … }`) targeting the bound local; `hoist_suspensions` runs a
  second time after the value desugars (they bind branch values whose suspensions can sit NESTED,
  e.g. in a constructor argument — `IrExpr::New` arguments hoist like call arguments now).
  (`tests/suspend_try_catch_shapes_e2e.rs`;
  `tests/suspend_e2e.rs::suspend_try_catch_without_a_suspension_runs`.) The `gate:suspend-try-catch`
  above is UNCHANGED — it guards the separate locals-restore loss, so a `try` over a REAL suspension
  with a value live across it still skips (`tests/suspend_e2e.rs::
  suspend_try_catch_over_a_suspension_still_skips`; corpus
  `coroutines/suspendFunctionAsCoroutine/handleException` remains skipped by it).
- **A never-entered branch emits NO body — the folded jump makes what follows it dead.**
  `emit_cond_branch` folds a constant condition: an always-taken test becomes an unconditional `goto`
  and an always-failing one emits no branch at all. Every instruction after an unconditional `goto` is
  reachable only by a jump, so it needs a stack-map frame; the never-taken branch has none, and the
  verifier rejects the method outright ("Expecting a stack map frame") rather than ignoring the dead
  code. `emit_cond_branch` therefore REPORTS whether it emitted the jump unconditionally, and both
  callers emit nothing on the path that follows: `emit_when` skips a branch whose condition folds to
  `false`, and the loop emitter skips the whole body/update/back-edge of a `while (false)`. kotlinc
  emits no body for a never-entered loop either. A post-test `do … while (false)` is unaffected — its
  body always runs once and only the folded back-edge disappears.
  Skipping the CODE must not skip the MERGE-POINT accounting. `diverges` deliberately does not fold
  constant conditions, so a `when` whose only falling-through branch is the dead one still reports as
  falling through and the caller keeps emitting at the merge — which therefore still needs its frame.
  `emit_when` marks the merge reachable for a skipped non-diverging branch; without that,
  `if (FALSE_CONST) "a" else return "b"` merely moved the same VerifyError from the dead body to the
  merge. (Binding a `val` to an `if` whose branches ALL diverge is a separate, pre-existing IR-backend
  refusal — a clean skip, not a miscompile, and not specific to constant conditions.)
  (`tests/empty_loop_body_e2e.rs::never_entered_while_emits_no_body`,
  `::never_selected_when_branch_emits_no_body`, `::never_selected_branch_still_frames_the_merge`.)
- **`suspend fun` — a cross-loop labeled `break`/`continue` compiles and runs.** A labeled jump leaving
  an INNER loop for an OUTER one used to produce an unverifiable method, and was refused
  (`suspending_cross_loop_labeled_jump`, now retired). The cause was the dead-branch defect above, not
  the flattener's jump routing: a `do … while (false)` whose own body never suspends is dragged into the
  state machine ONLY by such a crossing jump (`expr_jumps_to_active_frame`), and the flattener then gives
  it a header state holding `when (false) { goto body } else { goto exit }` — the literal condition the
  source wrote. `loop_targets`/`loop_jump_target` always picked the right target state; the emitter's
  never-taken `goto body` branch is what carried no frame. This is why only the POST-TEST form appeared
  broken: `do … while (false)` is the idiomatic never-repeating loop, so its header condition is a
  constant, while a pre-test `for`/`while` cross-loop jump normally tests something dynamic. A post-test
  loop with a NON-constant condition never failed, and a pre-test `while (false)` fails identically
  outside any suspend body. Verified against kotlinc for `break@outer`, `continue@outer`, a suspension in
  the inner body, and three nesting levels
  (`tests/suspend_e2e.rs::suspend_cross_loop_labeled_break_runs`,
  `::suspend_cross_loop_labeled_continue_and_three_levels_run`,  `::suspend_cross_loop_labeled_jump_between_pretest_loops_runs`; corpus
  `coroutines/controlFlow/doubleBreak`).
- **`suspend fun` — a suspension's RECEIVER/ARGUMENTS are evaluated into temps BEFORE the spill.** The
  spill stores used to be emitted ahead of the call, so an argument's update to a spilled local
  (`foo(i++)`) landed in the local but never in the field, and the resume restored the PRE-evaluation
  value — `bars(foo(i++), foo(i++))` silently answered `"1;1;"` instead of `"1;2;"`. kotlinc has its
  arguments on the operand stack before its `putfield`s, so its spill always observes the
  post-evaluation state. `bind_operand_temps` reproduces that ordering in IR: it binds the suspension
  point's receiver and each argument to a fresh temp emitted ahead of `spill_scope`, left to right so
  source evaluation order is preserved, and rewrites the call to read the temps. The temps never cross
  the suspension — the call IS the suspension and consumes them before it — so they get no spill slots.
  The emitted sequence matches kotlinc's modulo krusty's use of a local slot where kotlinc keeps the
  value on the operand stack (`iinc` then `putfield`, not `putfield` then `iinc`), verified by
  disassembling `bars(foo(i++), foo(i++))` against the reference compiler.
  Each temp is typed from the CALLEE's corresponding parameter — the `Local`/`MethodCall` target's
  `IrFunction::params`, `Callee::CrossFile`'s `params`, `Callee::Virtual`'s `params` when it carries them
  and its `descriptor` otherwise, or a `Callee::Static`/`Special` descriptor — and the receiver from the
  callee's `owner`, so the temp's store/load is the JVM kind the call consumes. Parameters are INDEXED,
  never length-matched, which is sound only while the surplus parameter is the TRAILING one: the callee
  signature may already carry the CPS `Continuation` that `append_continuation` appends to the arguments
  only after the spill. It is NOT trailing for a `$default` synthetic, whose descriptor spells the
  `Continuation` BEFORE the `int mask` + `Object marker` (`append_continuation` inserts the continuation
  VALUE two before the end for that reason), so zipping it would pair the mask with the `Continuation`
  slot and `astore` an int — every `$default` callee is refused instead.
  Binding fires only when an operand actually writes a local this point spills — every suspension would
  otherwise gain store/load pairs kotlinc does not emit — and a scratch written only inside the operand
  (`foo(run { t = 2; t })`) is unaffected either way. Shapes that cannot be re-bound are still REFUSED
  rather than reordered blindly: an intrinsic callee, an `inline` `Callee::Static` (spliced from its
  operand nodes, not called with them), a `Callee::Static` carrying a `dispatch_receiver` (which the
  non-splice emit path never pushes), any `$default` synthetic (`Callee::LocalDefault`, a `$default`
  `Callee::Static`, a `MethodCall` with omitted arguments), a `Lambda`/`Vararg` operand, and a
  conditional suspension buried in an operand (hoisting it would put a suspension ahead of this one's own
  spill). An inline-spliceable `Callee::Virtual` is deliberately NOT refused: the splice reads its
  operand nodes as values, and a `GetValue` of a temp is one. This ordering bug was the actual cause of
  the corpus `suspendCallsInArguments` divergence — a silent wrong answer in an otherwise-accepted shape,
  not a spill-slot displacement
  (`tests/suspend_e2e.rs::suspend_call_whose_argument_writes_a_local_runs`,
  `::suspend_member_call_whose_operand_writes_a_local_runs`,
  `::suspend_operand_write_to_a_locally_dead_scratch_runs`).
- **`suspend fun` — hoisting a suspension out of a call/template operand list preserves left-to-right
  evaluation.** Kotlin evaluates a call's receiver and arguments (and a string template's parts)
  strictly left to right; kotlinc spills every operand of a call with a suspending operand.
  `hoist_expr` used to rewrite only the SUSPENSION to a preceding temp, so `f(g(), susp())` became
  `val t = susp(); f(g(), t)` — running `g()` AFTER the suspension. `hoist_operands_in_order` now
  binds every runtime-read/evaluated operand that precedes a later suspending operand to a prelude temp
  first (only literal constants and the singleton value of a `Null`-typed local commute, per
  `operand_needs_snapshot`), for the
  `Call`/`MethodCall`/`StringConcat` arms of `hoist_expr`, the suspension-point path itself
  (receiver included), and the `hoist_stmt` arms that keep a direct `val r = <suspend call>` /
  bare-call statement (whose nested suspending arguments previously reached emit unhoisted and
  skipped the file — corpus `coroutines/controlFlow_chain.kt` now compiles). The snapshot plan is
  typed on the ORIGINAL operands before any rewrite (`hoisted_value_ty`; head kinds are preserved by
  hoisting), so an untypeable operand bails with the IR untouched and the flattener declines the
  shape — skip, never a reorder and never a double evaluation. An external callee
  (`Callee::External`) has no signature in the IR; its snapshot type comes from
  `ir.logical_types`, accepted only where logical = physical representation (scalars and `String`,
  e.g. the flattened `String.plus` chain whose intermediate accumulators ir_lower now records).
  The conservative boundary includes more than calls: ordinary local/parameter reads snapshot because
  an inline-spliced later block may write the same local before the residual call reads it; static reads
  snapshot regardless of source/module/classpath origin; and a wrapper that can THROW (`!!`,
  `as`/non-null cast, an unboxing `ImplicitCoercion`) snapshots so its exception precedes any later
  suspension's effects. Snapshot types come from identities already carried by the IR node
  (`GetStatic`/`GetField`/`RefGet`, static instances/fields/enums, and `PropertyRead`'s inline type except
  `Unit`/type-parameter reads); external field descriptors use the emitter's shared descriptor parser,
  rather than a suspend-specific classpath branch. This covers both `h.svc.m(susp())` and
  `f(x, run { x = 5; susp() })` without syntax- or provider-specific repair logic.
  Operand snapshotting can also make a previously post-suspension local read disappear. Positional
  scope capture still stores/restores every named local in scope, so
  `reconcile_positional_spill_locals` unions those actual spill consumers into the machine-local
  allocation set after named scopes and live temps merge. Both named-function and suspend-lambda
  machines use that boundary; no resume arm can restore a scope-only local into an undeclared slot.
  The existing `Nothing?`/`Ty::Null` rematerialization remains the semantic exception: such a local can
  only ever read as `null`, so it commutes without a snapshot and stays on the dedicated no-field
  rematerialization path instead of acquiring a verifier-sensitive ordinary temp.
  Ordering pinned by box runs against a real suspension (`yield()`), including the snapshot temp
  surviving the spill, the pre-mutation `var` read, the `!!`-throws-before-suspension case, and an
  effectful operand between two suspensions (`tests/suspend_arg_order_e2e.rs`, all ten shapes).
- **`suspend fun` — an INTRINSIC suspension point needs no operand temps.** A
  `suspendCoroutineUninterceptedOrReturn { c -> … }` recorded in `ir.intrinsic_suspension_points` is an
  inlined BLOCK, not a call: it has no operands to move ahead of the spill, and its body runs after the
  spill by construction (as in kotlinc, which has nothing on the operand stack there either). That is
  not the ordering hazard above, because a mutable local the block writes is captured BY REFERENCE — the
  front end `RefNew`-boxes it as soon as a lambda writes it — so the write lands in the heap cell whose
  reference the spill stored, and the restore cannot undo it. `bind_operand_temps` still refuses an
  operand-less point whose subtree writes a spilled local, so the property is enforced rather than
  assumed. (That `RefNew` shape is independently refused today by `box_returns`; kotlinc answers
  `"a;2;"` for it.)
- **`suspend fun` — a `Nothing?` local live across a suspension is REMATERIALIZED, not spilled.**
  `var x = null` has exactly one possible value, so kotlinc gives it no continuation field and re-emits
  `aconst_null; astore` in each resume arm. Spilling it instead is wrong twice over: the local's
  verification type widens from `null` to the field's `Object`, so the next typed use of it
  (`bar(x: String?, …)`) fails with "Bad type on operand stack", and the extra field diverges from
  kotlinc's count. `is_rematerialized_null` keeps such a local out of the spill layout and out of
  `kind_positions`, and each arm restores it with `Const(Null)`. Continuation fields are then identical
  to kotlinc's for the corpus `varSpilling/nullSpilling` shape (`L$0` for the crossing `String` temp,
  `result`, `label`)
  (`tests/suspend_e2e.rs::suspend_bottom_typed_local_across_a_suspension_is_rematerialized`).
- **`suspend fun` — a top-level `suspend` EXTENSION function.** `suspend fun Counter.next(): Int` needs
  no CPS machinery of its own: an extension receiver is already lowered to an ordinary LEADING static
  parameter, so the coroutine pass appends the `Continuation` after it (`next(Counter, Continuation)
  Object`) and threads call sites like any other static suspend call. The only thing missing was the
  registration — pass 1b's extension branch never pushed the `FunId` into `ir.suspend_funs`, so the
  pass saw neither the declaration nor its call sites (the call site then kept its pre-CPS arity: "call
  arity mismatch"). Registering it there dropped the BLANKET form of the `gate:extension-suspend-fn`
  file skip, leaving two narrower skips behind the same label.
  Proven: `suspend fun Counter.next()` suspending on `bump(base)` → 42
  (`tests/feature_coverage_s_e2e.rs::suspend_extension_function_on_user_type`). Those two shapes were
  (1) an extension body that suspends on a MEMBER of its receiver: a member suspension resumes against
  the machine's `this`, which an extension has not got — its receiver is a parameter slot — so the
  resumed call would target the wrong instance (fixed by the "`suspend` EXTENSION called through an
  explicit receiver" entry ABOVE, which retires the separate label
  `gate:extension-suspend-fn-member-suspension`); and (2) an `inline suspend` extension (fixed by the
  SIBLING-FILE entry immediately below). Both are closed, so no `gate:extension-suspend-fn` skip of
  any shape remains — the label does not exist in `src/` and declaring a `suspend` extension no
  longer refuses its file, in longhand or through an operator convention (see the
  operator-convention suspension entry near the top of this section, and the "OPERATOR CONVENTION is
  a suspension point" entry below).
- **`suspend fun` — a SIBLING-FILE `suspend` extension call is a suspension point, and `inline` does
  not change that.** `inline suspend fun Int.plusOne()` was gated on the assumption that the body is
  SPLICED at its call sites, where the splice and the CPS rewrite would not compose. It is not: krusty
  never splices a cross-file inline extension (`lower_inline_fn_call` accepts only a SAME-file
  declaration), so the call site always emitted a real call — just the wrong one. The defect is not
  about `inline` at all; the identical silent wrong answer reproduces with `inline` removed.
  `ResolvedCall::ModuleExtension` carried no `suspend` flag, so two things went wrong at once: the call
  kept its LOGICAL descriptor (`LibKt.plusOne(I)I` against an emitted `plusOne(I, Continuation) Object`),
  and `ast_body_suspends` — whose extension scan knows only THIS file's declarations — classified the
  driving `suspend { … }` lambda as leaf, so it got no state machine. The resulting `NoSuchMethodError`
  is swallowed by the driving `Continuation`, so `suspend { r = 1.plusOne() }.startCoroutine(EC())`
  answered `"fail"` instead of failing loudly. `ModuleExtension` now carries `suspend`; the cross-file
  branch registers the node in `ir.suspend_calls` so the coroutine pass rewrites the descriptor and
  threads the continuation (the same-file branch needs nothing — its callee is a local `FunId` already
  in `suspend_funs`), and `ast_body_suspends` consults the flag through
  `resolved_module_extension_suspends`. kotlinc is the oracle: for this repro it emits the CPS
  `plusOne(int, Continuation)` PLUS a private `plusOne$$forInline` copy, and splices only WITHIN the
  declaring compilation — so a sibling-file caller going through the real CPS entry point matches its
  ABI, and an `inline` declaration needs no separate emitted form. A SAME-file `inline suspend`
  extension is still declined loudly by the generic suspend-shape bail, never silently. Proven:
  `tests/cross_file_inline_call_e2e.rs::suspend_inline_extension_cross_file_executes`,
  `::suspend_extension_cross_file_executes` (the non-`inline` sibling — the latent miscompile the gate
  never covered), and `::suspend_extension_cross_file_with_suspension_point_executes` (the callee body
  itself suspends, so the resumed value must still reach the caller's assignment).
- **`suspend fun` — an OPERATOR CONVENTION is a suspension point even though it has no call node.**
  The sibling-file fix above was not enough for `suspend operator fun Box.get`/`set`/`plus`/
  `plusAssign`/`unaryMinus` reached through their conventions (`b[i]`, `b[i] = v`, `b + 1`, `b += 1`,
  `-b`): every suspension scan in `ast_body_suspends` keys on a call SHAPE — a bare `Name` callee, a
  `Member` callee, an `Expr::Call` node — and a convention has none of them. The checker records the
  selected target against the `Expr::Index`/binary node (`resolved_calls`), against the desugared
  operator (`resolved_operator_calls`), or against the assignment STATEMENT
  (`resolved_stmt_operator_calls`, and `StmtLowering::PlusAssign` for `+=`). The driving lambda was
  therefore classified as leaf and the same silent wrong answer followed. The scan is now SHAPE-FREE:
  `collect_nodes` walks every expression and statement under the body, and `ResolvedCall::suspends`
  answers for any target kind, surfaced through `resolved_call_suspends` /
  `suspending_operator_exprs` / `suspending_operator_stmts`. Lowering needed one addition beyond the
  shared `ModuleExtension` arm (`lower_op_call`/`lower_stmt_op_call` already route through it):
  `CompoundAssignmentTarget` dropped the flag entirely, so `Member`/`SourceExtension` now carry
  `suspend` and all three `lower_plus_assign` arms register the node. A cross-file
  `operator fun Box.compareTo` and `contains` are reached too, with and without `suspend` — `a < b`
  and `x in b` both resolve and run across the file boundary. `invoke` now consumes the same exact
  checker-selected extension target too: both ordinary and suspending `a()` calls declared in a
  sibling file resolve, emit, and run. Proven:
  `tests/suspend_operator_convention_cross_file_e2e.rs` (one test per convention, plus
  `::compare_to_and_contains_cross_file_execute` and
  `::invoke_convention_cross_file_executes`).
- **`suspend fun` returning a `@JvmInline value class` — the result crosses the CPS boundary BOXED.**
  A CPS return is `Object`, so a non-null value-class result cannot ride in its erased underlying form:
  kotlinc emits `X.box-impl` before the `areturn` and `checkcast X` + `X.unbox-impl()` on the resume
  side. The value-class pass runs BEFORE the coroutine pass and erases `X` to its underlying everywhere,
  so it now boxes such a suspend function's tail (the same `box_ref_tail` a lambda's erased `Object`
  result uses) and records the class in `ir.suspend_boxed_value_class_returns`; the coroutine pass's
  `bind_from_r` consults that record and unwraps the box instead of applying the ordinary
  `Object`→declared-type coercion. Value-class knowledge stays in the value-class pass — the record
  carries only the erasure it deliberately did NOT apply. Byte-identical to kotlinc for
  `suspend fun distance(): Meters` (`constructor-impl` → `box-impl` → `areturn`). A CROSS-UNIT suspend
  call whose logical return is a value class (`ir.suspend_calls`, a callee in another file with no such
  record) still skips the file. Proven: `runBlocking { compute() }` where `compute` binds `distance()`
  and reads `m.v` → 42 (`tests/feature_coverage_s_e2e.rs::suspend_returns_value_class`).
- **`@Metadata` writer — the suspend round-trip.** krusty now emits a `@kotlin.Metadata` annotation on
  a file facade that has top-level `suspend fun`s, so its OWN compiled output is consumable as a
  classpath dependency (a suspend fn's physical method is `Object foo(…, Continuation)` — only
  `@Metadata` carries `IS_SUSPEND` + the logical return). `metadata/builder.rs` writes the `Package`
  protobuf (`Function.flags` = `IS_SUSPEND | public | final` = 8198, the LOGICAL `return_type`, and the
  physical `JvmMethodSignature` extension), the backend builds it from the resolved `Signature`s and
  attaches it via `ClassWriter::set_kotlin_metadata` (`k=2`, `mv=[2,4,0]`, `xi=48`; `d1` is the payload
  one byte per `char`). Emitted only for facades with suspend functions (non-suspend facades resolve
  from their physical descriptors, unchanged). Proven both directions: krusty compiles a `suspend fun
  helper` lib, then krusty resolves + runs a caller against it → 43
  (`tests/suspend_e2e.rs::krusty_compiled_suspend_dep_is_consumable`); the real kotlinc 2.4.0 also reads
  the annotation and compiles the same caller without error.
- **`@Metadata` writer — plain top-level PROPERTY records.** The facade `Package` proto used to
  record extension properties only; plain top-level `val`/`var`s resolved through krusty's OWN
  static-field fallback, but the REAL kotlinc resolves an `import demo.greeting` exclusively from a
  `Package.property` record, so it reported `unresolved reference` against krusty-built libs.
  Every plain top-level property now gets a record mirroring kotlinc's observed encoding (verified
  by decoding kotlinc 2.4.0 output; matrix in `docs/METADATA_NOTES.md`): `flags` (f11, elided at
  the 518 wire default) composed from visibility bits, `IS_VAR|HAS_SETTER`, `IS_CONST|HAS_CONSTANT`
  for `const val`, `HAS_CONSTANT` for a `val` with a compile-time-constant initializer (never for a
  `var`), `IS_LATEINIT`; `getter_flags`/`setter_flags` (f7/f8) only for CUSTOM accessor bodies
  (visibility | `isNotDefault`); a custom setter's value parameter (f6); and a
  `JvmPropertySignature` naming exactly the accessors the emitter really produces — none for
  `const` (inlined) or `private` (direct field access; the synthetic `access$…$p` bridges are not
  metadata surface), and a backing-field entry (f100.f1) present iff the property has an
  initializer or is `lateinit`, carrying an explicit desc only when a nullable primitive boxes the
  stored type (`var e: Double?` → `Ljava/lang/Double;`). Extension-property records were aligned to
  the same observed shape (no backing-field entry, not-default accessor flags, f11 elided for a
  plain `val`). Delegated properties (`by lazy`) still emit no record — a tracked gap. Proven by
  `tests/top_level_property_e2e.rs::kotlinc_consumes_krusty_top_level_property_metadata`
  (previously `#[ignore]`): kotlinc 2.4.0 imports, compiles and runs against the krusty-built lib.
- **`@Metadata` writer — top-level `typealias` records.** A `typealias Name = Target` exists ONLY
  in metadata (`Package.typeAlias` = 5, `{name=2, underlying_type=4, expanded_type=6}` — decoded
  from kotlinc 2.4.0); krusty emitted nothing, so a consumer reported `unresolved reference` for
  the alias. Plain classifier aliases now emit records (underlying = expanded, the resolved
  target); a facade with ONLY aliases gets its `@Metadata` too. Function-type aliases
  (`type_alias_fun`) remain a tracked gap. The builder's class-id interning is now DEDUPED
  (repeated references share one d2 slot, as kotlinc does).
- **Generic-value-class members: the object-erased decline is narrowed to the real miscompile.**
  `build_class_metadata` declined ANY member whose value-class type erases its carrier to `Object`
  (a generic `TokenBox<T>`), so `class Factory { operator fun invoke(): TokenBox<String> }` got NO
  class `@Metadata` at all and a consumer reported "expression is not callable". The recorded
  rationale applies specifically to krusty's CPS BOXING divergence
  (`suspend_boxed_value_class_returns`): a NON-suspend member returns the raw carrier
  byte-identically to kotlinc (verified: `constructor-impl; areturn` on both sides, same mangle
  hash), so those members are now described; only a CPS return krusty boxes still disqualifies
  (and erased PARAMS keep declining pending the same verification). All four decline branches now
  emit a `trace_compiler!("emit", …)` reason — the silent-None debugging cost an hour.
- **Declared visibility reaches `@Metadata` (and ctor JVM access).** krusty hardcoded PUBLIC into
  every metadata flags word, so a consuming module could not enforce `internal`/`protected`
  boundaries against krusty-built libs. Now carried per declaration: `Class.flags` (an
  `internal class` writes explicit visibility 0 — `IrFile::class_visibilities`), `Function.flags`
  for top-level fns (`FnMeta::visibility` from the signature) and members
  (`IrFile::internal_methods`, alongside the dispatch-relevant `private_methods`),
  `Constructor.flags` for a DECLARED primary-ctor visibility (`class C protected constructor(…)` →
  4, private → 2 — `IrFile::ctor_visibilities`), and `TypeAlias.flags` (`internal typealias`,
  parser now keeps the modifier in `File::type_alias_visibility`). A declared ctor visibility also
  reaches the JVM `<init>` access flags (protected/private; `internal` stays JVM-public, the
  boundary lives in metadata alone), including the all-defaults convenience `<init>()`, which
  mirrors the primary's visibility exactly as kotlinc emits it.
- **Companion member properties — accessors + records.** A companion property's backing field is a
  static on the OUTER class; kotlinc realizes the property as `public final` INSTANCE accessors on
  `C$Companion` (via `access$…$cp` bridges over its private fields) and records it on the
  COMPANION's `@Metadata` (kind 390 class). krusty emitted only the outer static — no accessors,
  no records — so a cross-module WRITE of a companion `var` had no setter to resolve. Every
  companion property now registers under `declared_class_statics[C$Companion]`: the companion's
  class metadata gets a Property record per member (accessor signatures for non-const,
  `hasConstant` for constant-initialized `val`s — matching kotlinc's 8710/1798 words, d2
  byte-identical on the probe), and emission synthesizes the delegating accessors (direct
  `getstatic`/`putstatic` of the outer field, which krusty still emits public — the `access$…$cp`
  bridge + private-field shape is the remaining byte-parity delta). `const val`s stay
  accessor-less (inlined), as before. The module index's version
  words now match the reference toolchain (`[2,4,0]`, was the 1.9.24-era `[1,9,0]` — readable but
  byte-divergent), and the file is written UNCONDITIONALLY: kotlinc emits it with an empty parts
  list for a class-only module, so omitting it diverged the artifact set. Byte-identical against
  kotlinc for both the with-parts and empty shapes (unit tests pin the exact bytes).
- **`@JvmField` on companion-object properties.** Measured against kotlinc 2.4.10: the property is
  realized as a PUBLIC static field on the OWNER class (`final` for a `val`, non-final for a `var`;
  an `internal` declaration still gets a public unmangled field) with NO getter/setter anywhere and
  no `access$…$cp` bridges; the field carries the property's field-targeted annotations
  (`Lkotlin/jvm/JvmField;` first, then the nullability annotation) as `RuntimeInvisibleAnnotations`,
  and the owner's `<clinit>` initializes it after the `Companion` store, in declaration order. On an
  INTERFACE owner the field hoists onto the interface itself (`public static final`, `<clinit>` =
  `getstatic $$INSTANCE; putstatic Companion; …; putstatic <field>`), which kotlinc admits only when
  EVERY companion property is a `public final val` with `@JvmField` — krusty applies the same
  whole-companion rule and otherwise leaves the ordinary object-style storage. The companion's
  `@Metadata` property record keeps the `JvmField` annotation, drops the accessor signatures, writes
  the `var`'s default setter-flags word despite the missing physical setter, and (interface owner
  only) sets `JvmFlags.IS_MOVED_FROM_INTERFACE_COMPANION` (Property extension f101 = 1). The
  eligibility is one declaration-level ABI fact (`DeclaredPropertySig::is_jvm_field`, like
  `is_const`): plain stored `public`/`internal` property with an initializer — no custom accessor,
  delegate, `lateinit`, `const`, `open`, or `abstract`. Reads/writes from every distance go to the
  field directly: the checker records `StaticPropertyRead`/`StaticPropertyWrite` against the OWNER
  (same-file, sibling-file, and companion-instance reads alike), and classpath consumption falls
  back to the public field hoisted onto the companion's outer class when the companion class file
  carries neither accessor nor field (`companion_owner_field_access`) — this also fixed writes to a
  kotlinc-built `@JvmField var`. Hoisted companion statics are no longer registered in
  `declared_class_statics[owner]`: kotlinc's OWNER metadata carries NO record for a hoisted
  companion property (the declaration belongs to the companion's metadata alone), and the bogus
  const-flavored owner record was the last owner-class byte divergence. Byte-identical to kotlinc
  for owner + companion in the class-owner (val + var), internal-val, and interface-owner shapes
  (also under `-jvm-default=no-compatibility`); runtime round-trips same-file, cross-file, and
  cross-module (`tests/jvmfield_companion_e2e.rs`). Ineligible placements FALL BACK TOGETHER: the
  checker's routing and the JVM pass's hoist consult mirrored eligibility, including the two shapes
  where they could split — an interface companion MIXING `@JvmField` with a `const val` (the
  whole-companion check spans const statics too, which live outside `IrClass::properties`) and a
  VALUE-CLASS-typed `@JvmField val` (the checker declines routing exactly where the pass declines
  the hoist) — both pinned by fallback runtime tests, since kotlinc rejects those sources but
  krusty's contract is the ordinary realization. A WRITE's receiver expression keeps its side
  effects (kotlinc evaluates `side().v = 7`'s receiver, then `pop; putstatic` — measured), while a
  bare classifier/companion receiver and every READ receiver are dropped (also kotlinc's shape,
  measured: the read's `getstatic` has no receiver call). The classpath outer-field fallback fires
  only when the companion's `@Metadata` declares the property AND the accessor that record names is
  absent from the companion class file (the reader derives conventional accessor names, so record
  presence alone cannot discriminate). Deliberately NOT implemented: kotlinc's rejection
  diagnostics (private/lateinit/const/custom-accessor placements simply keep the ordinary
  realization; kotlinc rejects those sources outright), instance (constructor-property) `@JvmField`,
  and `@JvmField` on a named `object`'s properties, which keeps the previous private-static +
  accessor realization.
- **`@Metadata` writer — the CLASS round-trip (a `@Metadata` on every emitted class, not just the
  facade).** A file facade's `@Metadata` describes that file's TOP-LEVEL declarations only, so krusty
  used to emit nothing at all for a CLASS — and a krusty-compiled class was therefore unreadable by
  krusty itself. The gap is not about missing bytecode: `javap -p` showed `copy`, `copy$default` and
  `componentN` in the class file all along. What only `@Metadata` can carry is the Kotlin-level facts a
  JVM descriptor cannot spell — a constructor's and a member's PARAMETER NAMES (so `p.copy(y = 4)`
  binds by label) and the `operator` mark on `componentN` (so `val (a, b) = q` destructures). Compiling
  `data class Point(val x: Int, val y: Int)` with krusty and a caller against it reported "named
  arguments are only supported for top-level functions and methods with named parameters" and "cannot
  destructure this type (no operator 'component1')"; the same caller against a kotlinc-built `Point`
  compiled, which localized the defect to the WRITE side. `build_class_metadata` (IR → `metadata::
  class_builder::build_class`) already existed and was byte-verified against kotlinc, but every in-tree
  caller left it switched off. It is now ON in the shipping emit configuration. It is NOT unconditional
  emission: a shape `build_class_metadata` has not verified declines individually and that class emits
  no annotation exactly as before (companion / annotation class / enum entry / property- and
  function-reference classes / secondary constructors / a non-interface without a primary constructor /
  a multi-field or `var` `value class`) — an unverified payload once broke kotlin-reflect on a
  box-corpus case, which is why it was gated at all. That list is a safety NET, not a proof: it gates
  on class KIND, so a describable kind can still hold a MEMBER the builder models wrongly. Each such
  shape has had to be found and added — the three below are the ones switching the default on
  surfaced, and the honest expectation is that more exist. Byte parity IMPROVES rather than regresses,
  since kotlinc annotates every class too.
  ONE DEFINITION of the shipping emit configuration (`jvm::backend::shipping_emit_options`) is what
  makes this reach every caller: the in-process test harness previously built its own `EmitOptions`
  from `Default`, which silently omitted both the class metadata AND the `SourceFile` stamp — so a
  test could pass on an artifact `krusty -d …` never writes. The CLI backend and `compile_in_process`
  now share that one constructor. `EmitOptions::default()` is NOT a pre-class-metadata escape hatch —
  its `emit_class_metadata` is `true` as well; what it lacks next to the shipping configuration is the
  `SourceFile`, the inner-class resolver and the `-jvm-target` class version, so a caller that reaches
  for it still gets class metadata and still is not emitting shipping bytes. The two supported ways to
  get facade-only output are `KRUSTY_NO_CLASS_METADATA` (consulted by `shipping_emit_options` only,
  for bisecting) and constructing `EmitOptions` explicitly with `emit_class_metadata: false`.
  A **`data object` synthesizes no `copy`/`componentN`** — it is a singleton, so kotlinc gives it
  `equals`/`hashCode`/`toString` only. krusty's METHOD emission already agreed, but the constant-pool
  seeder and the metadata builder both keyed on `is_data` alone, so switching the annotation on made a
  `data object` advertise a `copy()` its own class file does not define — a reader would have bound a
  call that then fails at link time. Both now ask `synthesizes_data_class_members` (`is_data &&
  !is_singleton`). This is the class of defect the gate could not see while the annotation was off:
  a wrong payload is only observable once something writes it (`tests/sealed_interface_nested_e2e.rs::
  data_object_has_no_copy`, extended to decode the emitted `@Metadata`).
  **`data` synthesizes over the PRIMARY-CONSTRUCTOR properties, not over every field.** `c.fields` also
  holds a BODY property's backing field, so `data class P(val x: Int) { val y: Int = 1 }` was described
  with `component1`, `component2` and `copy(II)LP;` while the class file defines only `component1` and
  `copy(I)LP;` — krusty's METHOD emission was right and matched kotlinc; only the record was wrong.
  Real kotlinc reading it accepts `val (a, b) = p` and binds a `component2` that does not exist. The
  builder and the constant-pool seeder now both take the `c.ctor_param_count` prefix, which makes the
  `d2` string table byte-identical to kotlinc's for this source (`krusty_roundtrip_class_metadata_e2e::
  a_body_property_adds_no_component_or_copy_parameter`).
  **Admission is TRANSITIVE: a class is not described in terms of a value class a reader cannot read
  back as one.** A value class without a record reads downstream as an ordinary class — the caller
  casts the carrier to the box and binds an INSTANCE accessor where kotlinc emits the static `-impl`
  (`A.create('O').publicValue` → `checkcast A; A.getPublicValue()` on a `String`, ClassCastException).
  So `value_class_is_readable` answers POSITIVELY, never by assumption: a value class declared in THIS
  file must pass the builder's own shape bails (`value_class_metadata_shape_admitted` — kind, single-`val`
  field, no value-class ctor parameter, nothing declared beyond the synthesized set); one declared in
  another file of this MODULE is unknown here, because its record is decided by its own emit, so the
  answer is no; anything else is on the CLASSPATH, where value-class-ness is itself decoded from the
  `@Metadata` inline record — being known as a value class at all IS the evidence a record exists.
  That is what lets `Factory.invoke(Result<Int>)` be described while `Holder.make(): A` (a
  sibling file's value class with a declared member) stays withheld. The value classes the pass
  resolved reach the writer through the existing `IrFile::is_value_class_name` lookup plus the
  `module_source_value_classes` origin subset; there is no second value-class name table.
  Found by the box corpus's
  `compileKotlinAgainstKotlin/inlineClasses/privateConstructorWithPrivateFieldUsingTypeTable`; the
  cross-file half by review. Test:
  `krusty_roundtrip_class_metadata_e2e::a_sibling_files_undescribed_value_class_withholds_the_record`.
  Still open, and PRE-EXISTING (this entry neither caused nor fixed it): a value class whose carrier is
  `Object` read out of a generic slot is not unboxed — `val w: W = listOf(W("a"))[0]; showW(w)` passes
  the box where the carrier is wanted, and passing `list[0]` STRAIGHT into a value-class parameter
  fails the same way for a concrete carrier too (nothing unboxes a boxed argument at the call itself).
  **With the read side in place, a VALUE-CLASS-INVOLVED member and a VALUE-CLASS-typed BODY PROPERTY
  are both DESCRIBED, byte-identically to kotlinc.** The member is stated in Kotlin terms
  (`make(): K`) with a `JvmMethodSignature` carrying the mangled name and the erased descriptor —
  `ir.vc_declared_sigs` holds that declared form, recorded before erasure. The BODY PROPERTY is the
  harder half: its accessor is synthesized straight from the declaration and never appears in
  `c.methods`, so the record takes the Kotlin type from the `IrProperty` (`k: LK;`) and the JVM
  spelling from the value-class pass's stamp (`getK-XLNMDGE`), plus an explicit
  `JvmFieldSignature.desc` — a reader cannot derive the erased `Ljava/lang/String;` field from the type
  `K`. ONE source of accessor spelling (`ir_emit::accessor_jvm_names`) now feeds the record, the
  constant-pool seeder, the debug tables and the `@NotNull` attachment, because all four key on the
  accessor by NAME: while three of them said `getK`, the record advertised a method the class file does
  not define, the pool interned a constant nothing referenced, and the real accessor silently lost its
  `LineNumberTable`/`LocalVariableTable` and its `@NotNull`. The seeder also interns a value-class
  initializer's `constructor-impl` between the constant it pushes and the field it stores, which is
  where kotlinc puts it. Tests: `data_class_metadata_wiring_e2e::{value_class_parameter_member,
  value_class_return_member, value_class_body_property, suspend_returning_nullable_value_class}
  _is_byte_identical`, and the round-trips
  `krusty_roundtrip_class_metadata_e2e::{a_value_class_returning_member, value_class_body_property}
  _round_trips`.
  The same one-source rule fixed an `is`-prefixed property: `val isOpen` keeps the SOURCE name as its
  accessor (`isOpen()`, never `getIsOpen`; a `var`'s setter is `setOpen`), and the record used to name
  `getIsOpen` — a method the class file does not define — while the accessor lost its debug tables.
  Test: `data_class_metadata_wiring_e2e::is_prefixed_property_accessors_are_byte_identical`.
  **Still open, same family: a VALUE-CLASS-typed CONSTRUCTOR PARAMETER withholds the record.** Not for
  the property's sake — that half is described correctly now — but for the CONSTRUCTOR's: the class
  gets kotlinc's private-primary + synthetic `DefaultConstructorMarker` ABI, and the builder names the
  PRIVATE `<init>(Ljava/lang/String;)V` where kotlinc names
  `(Ljava/lang/String;Lkotlin/jvm/internal/DefaultConstructorMarker;)V`. Real kotlinc reading that
  record rejects `Holder(ItemId("OK"))` as a type mismatch, and a caller that satisfied it would
  `invokespecial` a private constructor. A `value class` with a DECLARED member also still declines:
  its member runs on the unboxed carrier through a static `-impl` pair the record does not yet spell.
  `ir.has_value_param_ctor` (recorded before erasure) is the signal. Test:
  `krusty_roundtrip_class_metadata_e2e::a_value_class_constructor_parameter_withholds_the_record`.
  **The classpath value-class RETURN, and why a VALUE-CLASS-INVOLVED member can now be DESCRIBED.**
  A value-class return erases exactly like a value-class parameter: the JVM method hands back the
  UNDERLYING (`fun make(): K` → `make-XLNMDGE()Ljava/lang/String;`) while `@Metadata` names `K`. A
  call site needs BOTH halves. `MetadataCallFacts` carried only `value_class_params`, so a caller
  learned the Kotlin return and boxed as kotlinc does at a genuine box boundary —
  `invokevirtual Holder.make-XLNMDGE()Ljava/lang/String; checkcast K; K.unbox-impl()` — over a
  `String` that already IS the carrier (`ClassCastException: class java.lang.String cannot be cast to
  class K`). The record was therefore withheld, and every caller stayed on the descriptor path.
  The model has three parts.
  **`MetadataCallFacts::value_class_ret`** (`value_class_return_type`, mirroring
  `value_class_param_types`) reports the value class a descriptor return really has: the metadata
  names a value class, it is NON-nullable, and the descriptor carries exactly that class's underlying.
  A nullable value class is genuinely BOXED, so `metadata_value_class_underlying` returning `None`
  for it is what keeps it on the boxed path. `jvm_libraries` applies it as the non-suspend return
  (previously `physical_ret` outright, so a top-level `(): Duration` read back as `Long`), keeping
  `physical_ret`/`descriptor` erased.
  **`LibraryMember::declared_ret` / `LibraryCallable::declared_ret` → `IrFile::call_declared_ret`** is
  the return analogue of `source_receiver`: the callee's DECLARED, un-erased, pre-substitution return,
  forwarded verbatim by `ir_lower` (which does no value-class reasoning) and read by the value-class
  pass. The SUBSTITUTED type cannot serve. `List<TokenBox>.get` and `A.create(): A<String>`
  both present as "returns a value class, physically `Object`", yet the first hands back a BOX out of
  a generic slot and the second the erased carrier; only the DECLARATION separates them — `get`
  declares the type parameter `E`, `create` declares `A`. `value_classes::repr` consults it FIRST, and
  a declared value-class return is `Unboxed` whatever the underlying erases to.
  **`coerce_to_static` records the substituted type in `logical_types`** beside the physical one, so a
  `Cast` to the value class strips as redundant instead of reading as "an erased value narrowed to
  `K`". Scoped to a value-class static type AND a non-erased-top physical type: at `Object` the pair
  cannot classify the result, and recording it there unboxed real boxes
  (`TokenBox cannot be cast to java.lang.Integer`, four corpus cases).
  **What still declines, and why none of it is the return model.** Each is a WRITE-side divergence
  from kotlinc, invisible while the record was withheld, and each is proven by a differential
  comparison for the same source. (1) A VALUE-CLASS-typed CONSTRUCTOR PARAMETER, unchanged — see the
  paragraph above. (2) A VALUE class with a
  DECLARED MEMBER: kotlinc realizes `value class S(val v: String) { fun k(): String }` as the STATIC
  `k-impl(Ljava/lang/String;)Ljava/lang/String;` over the carrier, krusty as an INSTANCE `k()` on the
  box, so reading krusty's record puts the carrier under an `invokevirtual S.k()` — a VerifyError.
  The read side is fine there: against a KOTLINC-built `S` the same `box()` runs. A COMPUTED property
  counts as such a member and `declared_fids` cannot see it (its accessor is synthesized from
  `IrProperty`, and `accessor_names` comes from backing fields); the SOLE underlying property does
  not, since kotlinc gives it an instance `getV()` too. (3) A member whose value-class position erases
  to `Object` (`value class A<T>(val value: T)`, `kotlin/Result`). `call_declared_ret` now resolves
  the RETURN ambiguity on member, static and operator-invoke paths, but parameter positions still
  lack the equivalent selected-declaration carrier fact: an `Object`-underlying value-class argument
  may arrive boxed where the callee expects its carrier. Admission therefore remains a conservative
  whole-member decline whenever any declared value-class position erases to `Object`, until both
  directions are verified on every call route. The test is read from the ERASED signature rather
  than a value-class table, so it holds for a classpath value class exactly as for a same-file one.
  A `suspend` member's RETURN is exempt from
  this test — CPS makes it `Object` whatever it declares — with one exception that is a real
  miscompile: (4) a CONCRETE `suspend` member whose value-class return krusty BOXES at the CPS
  `areturn` (`ir.suspend_boxed_value_class_returns`). kotlinc boxes there only for a PRIMITIVE
  underlying; over a reference, nullable, or generic underlying it `areturn`s the raw carrier, while
  krusty boxes unconditionally. Because the record krusty writes is byte-identical to kotlinc's,
  describing such a member advertises an ABI the class file does not implement — a consumer doing
  `C().gk().v` gets "class K cannot be cast to class java.lang.String", and against a KOTLINC-built
  `C` the same source runs. An ABSTRACT suspend member has no return expression to box and never
  enters that table, which is why the suspend INTERFACE shapes stay describable. For the same reason
  `LibraryMember::declared_ret` is not set for a `suspend` member: CPS erases its descriptor return to
  `Object`, so the descriptor stops witnessing that the result is the carrier, and for a
  primitive-underlying value class it is not one (`make-<hash>(Continuation)Ljava/lang/Object;` hands
  back `M.box-impl(I)LM;`) — those fall back to the descriptor comparison, which classifies them
  correctly.
  Tests: `krusty_roundtrip_class_metadata_e2e::a_value_class_returning_member_round_trips`,
  `a_value_class_parameter_member_round_trips` and
  `an_inherited_value_class_returning_member_round_trips` each RUN `box()` against krusty's own class
  output (a caller that merely compiles while emitting the boxed form still fails);
  `a_value_class_with_a_declared_member_withholds_the_record` and
  `a_concrete_suspend_value_class_return_withholds_the_record` pin the declines above on the emitted
  METHOD, so each fails the day its ABI is corrected. `data_class_metadata_wiring_e2e::
  value_class_parameter_member_is_byte_identical`, `value_class_return_member_is_byte_identical` and
  `suspend_returning_nullable_value_class_is_byte_identical` assert the whole class file, `@Metadata`
  included, against kotlinc's.
  The box corpus's `// MODULE:` path — the only place the gate compiles a DOWNSTREAM module against
  krusty's own class output — now emits class metadata too, matching what ships; switching the
  annotation on was itself a net gain (3466 → 3471 → 3589 cases compiled as the value-class records landed, still 0 miscompiles), and
  describing value-class members took it from 3472 to **3587 cases compiled, still 0 miscompiles**.
  Keeping it off would have left the gate blind to precisely the defects above: they surfaced only
  once that path wrote what the CLI writes.
  One test had to be corrected before the default-on switch could pass, and the correction is the
  interesting part: it asserted that a plain enum carries NO `RuntimeVisibleAnnotations` attribute at
  all, which contradicts kotlinc — a kotlinc-compiled plain enum carries one, its own class-level
  `@Metadata` among them. It now asserts on the annotation TYPE (`Ldemo/Mark;`), which is what "the
  constants are not annotated" actually means
  (`enum_constant_annotation_emit_e2e::unapplied_annotation_leaves_no_trace_on_a_plain_enum`).
  Tests: `tests/krusty_roundtrip_class_metadata_e2e.rs` (the write side pinned by decoding the emitted
  `Point.class`, plus `copy(y = …)`/destructuring and a plain class's member named arguments
  round-tripping through krusty's own output), and the data-class half of
  `feature_coverage_x_e2e::roundtrip_data_class_and_generic_fn` — whose GENERIC half is a separate,
  facade-side rule (see "A facade `@Metadata` record keeps a BOUNDED type parameter as a type
  parameter").
- **An `annotation class` that declares `@Target` carries THREE meta-annotations, and the Java one is
  a PROJECTION.** kotlinc writes, into `RuntimeVisibleAnnotations`: every annotation the source
  declares, in SOURCE order (`kotlin.annotation.Retention` and `kotlin.annotation.Target` among them);
  then `java.lang.annotation.Retention`; then `java.lang.annotation.Target`. krusty emitted the two
  retention stamps FIRST and the source's own annotations after them, and never emitted the java target
  mirror at all. Source position matters and is not merely cosmetic ordering: `kotlin.annotation.
  Retention` is SYNTHESIZED from the class's `annotation_retention` rather than carried through as a
  written annotation, so it is now substituted IN PLACE of the source's `@Retention` instead of being
  filtered out and re-appended — `@Retention(BINARY) @Target(FIELD)` and the same pair written the
  other way round produce different classfiles, and both are pinned.
  The java mirror is not a copy of the Kotlin target set: each `AnnotationTarget` maps to at most one
  `ElementType` and the mapping is neither an identity (`CLASS` → `TYPE`, `ANNOTATION_CLASS` →
  `ANNOTATION_TYPE`, `VALUE_PARAMETER` → `PARAMETER`, `TYPE` → `TYPE_USE`) nor injective (`FUNCTION`,
  `PROPERTY_GETTER` and `PROPERTY_SETTER` all become `METHOD`). kotlinc collects the result in an
  `EnumSet<ElementType>`, so duplicates COLLAPSE and the entries come out in `ElementType` DECLARATION
  order, not the order the Kotlin targets were written: `@Target(TYPE, CLASS, VALUE_PARAMETER,
  ANNOTATION_CLASS)` mirrors to `[TYPE, PARAMETER, ANNOTATION_TYPE, TYPE_USE]`. The Kotlin-only targets
  (`PROPERTY`, `FILE`, `TYPEALIAS`, `EXPRESSION`) map to nothing — but the mirror is still EMITTED,
  with an empty array: `@Target(AnnotationTarget.PROPERTY)` yields `java.lang.annotation.Target(value =
  [])`. Only the ABSENCE of `@Target` omits the two target meta-annotations, and an explicit
  `@Target()` is a third, distinct shape. A `javap` grep for `ElementType` hides the empty-mirror case,
  which is why every row was measured per target against kotlinc 2.4.10 rather than derived. The table
  is `types::java_element_type_of_annotation_target` + `types::JAVA_ELEMENT_TYPES` (ordering); the
  emitter derives the mirror from the recorded `kotlin.annotation.Target`
  (`jvm::ir_emit::java_target_mirror`), so there is one source of truth.
  NOT yet byte-identical as a whole class: krusty emits no `@kotlin.Metadata` for an `annotation class`
  at all (`class_metadata_common_shape_admitted` bails on `is_annotation`), and it emits a
  `…$annotationImpl` class kotlinc only emits when the annotation is instantiated. Both are independent
  of `@Target` — they diverge with or without it, which is why the tests in
  `tests/annotation_target_emission_e2e.rs` compare the whole `RuntimeVisibleAnnotations` attribute
  against kotlinc's rather than the whole class file.
- **A VALUE-PARAMETER annotation is recorded TWICE, and both records are required.** `fun f(@Mark a:
  Int)` / `class C(@Mark val x: Int)`: kotlinc writes (1) a JVM
  `Runtime{Visible,Invisible}ParameterAnnotations` attribute on the method or constructor — RUNTIME
  retention, Kotlin's default, is the *visible* one — and (2) a `@Metadata`
  `ValueParameter.annotation` record (field 7, an `Annotation { id }` naming a `DESC_TO_CLASS_ID`
  string-table entry) plus `ValueParameter.flags` bit 0, `HAS_ANNOTATIONS` (so an otherwise-plain
  annotated parameter writes flags `1`; combined with `DECLARES_DEFAULT_VALUE` it is `3`). Same field
  number on a `Function`'s and a `Constructor`'s value parameters. krusty emitted NEITHER: the
  annotations were parsed onto `ast::Param`/`ast::PropParam` and never lowered. Three orderings are
  part of the contract and were all measured against kotlinc 2.4.10:
  *(a)* a parameter's annotation class id interns in `d2` AFTER that parameter's own name and type,
  and before the constructor's `JvmMethodSignature` (f100) strings. This is a per-FIELD rule, not a
  general "annotations before f100": a DECLARATION-level annotation (`Constructor.annotation` f3,
  `Function.annotation` f12, `Property.annotation` f14) interns AFTER f100, the opposite way round.
  Measured together in one fixture — `class C @OnCtor constructor(@OnParam val x: Int)` gives
  `d2 = [… "x", "", "Lp/OnParam;", "<init>", "(I)V", "Lp/OnCtor;", …]`, the parameter's f7 before the
  signature strings and the constructor's own f3 after them. So `append_param_annotations` must stay
  INSIDE the value-parameter loop with `jvm_method_sig` after it; moving either past the other shifts
  every later `d2` index;
  *(b)* in the constant pool the annotation DESCRIPTORS land at the method HEADER — after the name,
  descriptor and generic `Signature`, and before any `Code`/`LocalVariableTable` string — ordered
  return annotation, then every parameter's RUNTIME-retained type, then per parameter its
  BINARY-retained types followed by that parameter's synthesized `@NotNull`/`@Nullable`;
  *(c)* within `RuntimeInvisibleParameterAnnotations` the user annotation precedes the synthesized
  `@NotNull`, and the whole visible attribute precedes the invisible one;
  *(d)* `num_parameters` spans the method's PHYSICAL descriptor, not its declared parameter list. A
  `suspend fun f(@Mark a: Int)` compiles to `f(int, Continuation)`, and kotlinc writes
  `num_parameters = 2` with an EMPTY entry for the synthesized continuation rather than truncating the
  attribute at the annotated source parameter. krusty sized it from the source list and wrote `1`,
  describing a different parameter list; `set_method_param_annotations` now derives the count from the
  descriptor (`descriptor_param_count`) and pads.
  The frontend never CHECKED a function's value-parameter annotations on either the top-level
  (`check_fun`) or the member path, so no application was recorded and lowering had nothing to read;
  constructor parameters were already recorded and only lacked a consumer. Both function paths now
  record them on the same terms as the property and primary-constructor annotations — recorded but
  NOT diagnosed (`diags.truncate` around the loop) — because krusty's folder is narrower than
  kotlinc's and these applications were never checked before, so a diagnostic here would reject
  sources kotlinc accepts. Placement follows Kotlin's use-site defaulting rather than a guess: a
  constructor `val`/`var` parameter's annotation reaches the PARAMETER only when
  `AnnotationTargets::property_declaration_site` resolves to `ValueParameter`, leaving the property-
  and field-targeted ones to the marker method and the backing field; a plain function parameter has
  no such choice. Retention stays SEMANTIC through lowering (one ordered list per parameter) and is
  split into the two JVM attributes at emission via `split_declaration_annotations`, so the two
  halves cannot diverge. GAP: an annotation WITH ARGUMENTS is left out of
  `@Metadata` (`class_builder::records_annotation`) because the `Annotation.Argument.Value` model is
  not written yet; recording the class with its arguments dropped would describe a DIFFERENT
  annotation. The classfile attribute carries the full form either way. Tests: the six cases in
  `tests/annotation_emission_e2e.rs`: member function, constructor property, top-level facade
  function, and mixed RUNTIME+BINARY+`@NotNull` on one parameter are whole-class byte-identical; a
  `suspend` function's parameter is asserted on the decoded attributes instead (that shape has an
  unrelated pre-existing divergence — krusty boxes the `Int` result with `Integer.valueOf` where
  kotlinc uses `kotlin.coroutines.jvm.internal.Boxing.boxInt`); plus a `@Metadata`-only assertion.
- **`suspend` function TYPE representation (`suspend (A..) -> R`).** kotlinc realizes it as
  `Function{n+1}<A.., Continuation<R>, Object>` — the arity is the logical parameter count PLUS one (a
  trailing continuation), the result erased to `Object`. krusty historically dropped the `suspend`
  modifier on a function type and emitted `Function{n}` (a miscompile). Now `TypeRef.fun_suspend` (the
  parser already consumed but discarded `suspend` before a function type) flows to `FnSig.suspend` and
  `IrType::Function.suspend`, and the descriptor adds one to the arity (`suspend () -> Int` →
  `Function1`). A suspend-lambda LITERAL or any value passed to a suspend-function-type parameter still
  needs `SuspendLambda` codegen / continuation threading (not yet modeled), so those bail (skip the
  file) — never the prior `Function0`-vs-`Function1` miscompile. Proven by an ABI signature diff:
  `take(block: suspend () -> Int)` lowers to `void take(Function1)`
  (`tests/suspend_e2e.rs::suspend_function_type_lowers_to_function1_continuation`).
- **`SuspendLambda` codegen (leaf, no captures).** A `suspend` lambda literal (`{ 42 }`) flowing into a
  suspend function-type position compiles to a concrete class
  `… extends kotlin/coroutines/jvm/internal/SuspendLambda implements Function{n+1}` — NOT krusty's
  `invokedynamic`/`LambdaMetafactory` path (which can't realize the `SuspendLambda` ABI). The class has
  `<init>(Continuation completion)` → `super(n+1, completion)`, `invokeSuspend(Object result)` (the body,
  result boxed), and the erased `invoke(Object arg)` = `new This((Continuation)arg).invokeSuspend(Unit)`.
  The creation site is `new This((Continuation) null)` (the completion is supplied when the lambda is
  invoked). `lower_arg` routes a lambda bound for an `IrType::Function{suspend:true}` parameter to
  `lower_suspend_lambda`; any non-lambda suspend value still bails. Proven end-to-end:
  `make(): suspend () -> Int = { 42 }` returns a `Function1` a Java driver invokes with a continuation →
  boxed 42 (`tests/suspend_e2e.rs::leaf_suspend_lambda_creates_and_invokes`). **Captures**: a free
  variable the lambda reads becomes a `final` field set in `<init>(cap.., Continuation completion)` and
  copied into the fresh instance `invoke` builds (`new This(this.cap.., (Continuation)arg)`); the
  creation site passes the captured values (`new This(captureValues.., null)`). `invokeSuspend` loads
  each capture field into a local before running the body. Proven: `make(n: Int): suspend () -> Int =
  { n + 1 }`, `make(10).invoke(k)` → 11 (`::suspend_lambda_captures_enclosing_variable`). Own
  parameters use fields after the captures, populated by `create`/`invoke` and reloaded by
  `invokeSuspend`; parameters and captures may coexist. **Internal suspension**: a lambda whose body
  is a single TAIL suspend call (`{ foo() }`, `{ suspendOnce() }`) compiles its `invokeSuspend` to a state machine with the
  lambda instance itself as the continuation — a `label` field on the class, dispatch on `this.label`:
  state 0 threads `this` (cast `Continuation`) into the callee and sets `label=1` (a classpath/sibling
  callee, resolved by its logical signature, gets its descriptor rewritten to the CPS form here), then
  returns `COROUTINE_SUSPENDED` up if the callee suspends else the value; state 1 (the async resume,
  re-entered by the callee's `resumeWith`) returns the resumed `result`. A suspending body that isn't a
  supported state-machine shape still bails rather than emitting partial CPS. Lambda-suspension
  detection walks AST call identities and reads each checker's exact provider-neutral `ResolvedCall`
  (same-file, sibling-module, and classpath alike); it never classifies by a same-named declaration.
  Proven both
  completion modes: `make(): suspend () -> Int = { foo() }` → 42 synchronously
  (`tests/suspend_e2e.rs::suspend_lambda_with_internal_suspension_runs`); `{ suspendOnce() }` against a
  real kotlinc parking primitive suspends then resumes to 42
  (`::suspend_lambda_internal_suspension_async_resume`). A **non-tail** body that BINDS the result and
  computes a tail expression (`{ val a = foo(); a + 1 }`) is handled: state 0 resumes into the binding
  (`a = unbox(callResult)`) and runs the tail; state 1 binds `a` from the invokeSuspend `result` and
  runs the same tail. Limited to a SINGLE suspension; the invokeSuspend body is lowered with
  `next_value` reset to 2 (`this`=0, `result`=1) so the bound local can't collide with the machine's
  marker/result temps. Proven: `{ val a = foo(); a + 1 }` → 43 (`::suspend_lambda_non_tail_body_runs`).
  **Multiple suspensions / control flow** use the GENERAL lambda-mode machine: ir_lower builds
  `invokeSuspend` with the plain body and registers `(FunId, ClassId, field_base)` in
  `ir.suspend_lambda_sm`; the coroutine pass's `build_lambda_state_machine` reuses the same `Flat`
  flattener as functions — the continuation is the lambda instance (`cont_v = this`, value 0), its
  `result`/`label`/spilled fields are appended to the lambda class after the captures/params
  (`field_base`; `Flat.setfield` adds it), and `invokeSuspend` stores its `result` parameter into the
  `result` field at entry, then loops `while(true){ r = this.result; <restore spilled>; when(this.label){
  states } }`. Proven both completion modes incl. spilling a value across a second suspension:
  `{ val a = foo(); val b = bar(); a + b }` → 142 synchronously (`::suspend_lambda_two_suspensions_runs`),
  and `{ val a = suspendOnce(); val b = plain(); a + b }` parks then resumes to 142
  (`::suspend_lambda_two_suspensions_async_resume`). A lambda that BOTH captures and suspends is handled
  by the same general machine: a capture is reloaded from its field into its local (value-index `2+i`)
  in the `invokeSuspend` PROLOGUE at every entry (so it survives a re-entry) and is excluded from
  spilling. Proven: `make(n: Int): suspend () -> Int = { val a = foo(); n + a }`, `make(10).invoke(k)` →
  52 (`::suspend_lambda_captures_with_suspension_runs`).
  **Own parameters** (leaf, no captures): a
  parameter is a field set when the lambda is invoked — `invoke(Object p.., Object completion)` builds a
  fresh instance `new This(this.cap.., (Continuation)completion)`, stores each `(paramType)p_i` into its
  field, then calls `invokeSuspend(Unit)`; `invokeSuspend` loads the param fields into locals bound to
  the lambda's parameter names. The class implements `Function{arity+1}`. Proven:
  `make(): suspend (Int) -> Int = { it + 1 }`, `make().invoke(10, k)` → 11
  (`::suspend_lambda_with_parameter_runs`). This is also the shape a coroutine-builder lambda takes
  (`runBlocking`/`launch` accept `suspend CoroutineScope.() -> T` — a receiver lambda is a 1-parameter
  suspend lambda), so builders are ordinary classpath calls once their suspend-lambda argument compiles.
  **Own parameters WITH captures**: the two are the same mechanism — captures are the leading fields,
  stored by the constructor from the creation site; parameter slots are the fields after them, stored by
  `create`/`invoke`; `invokeSuspend` reloads both. They are therefore modeled together, not just
  separately (the earlier leaf-only restriction was a scope limit, not a machine limit). Proven for a
  receiver slot plus a captured `var` (`withScope { seen += budget }`) and for a value parameter plus a
  capture, each box-run (`tests/suspend_receiver_lambda_e2e.rs::suspend_receiver_lambda_captures_and_receiver`,
  `::suspend_value_param_lambda_captures`).
- **A suspend lambda's parameter slots bind the RECEIVER as `this` — for a classpath callee too.** A
  `suspend R.() -> T` parameter folds its receiver into the erased `Function{n+1}`'s FIRST slot, and the
  checker resolves a bare member in the body against that receiver. Lowering binds the leading
  context/extension slots as the implicit `this` and the remaining slots to the lambda's own parameter
  names. Both spellings of a suspend function type now go through the one rule
  (`Lower::suspend_lambda_bind_names`): the source `suspend` marker, and a CLASSPATH parameter whose
  descriptor erases the marker away (recognized structurally by the trailing `Continuation`) — the
  erasure hides `suspend`, not the receiver, which survives as `@ExtensionFunctionType` in the callee's
  `@Metadata`. Previously the classpath path bound that slot as the value parameter `it`, so any body
  that actually USED the receiver failed to lower and the whole file was skipped ("this construct is not
  yet supported by the IR backend") while an empty body compiled. Proven against a kotlinc-built
  dependency, box-run: a receiver read, a capturing body, and a named argument ahead of the trailing
  lambda (`tests/classpath_suspend_receiver_lambda_e2e.rs`).
- **A `Unit` tail in a suspending lambda body runs for effect and yields the `Unit` singleton.** Several
  `Unit` tails leave NOTHING on the operand stack — a call (to a function, a method, or a function VALUE)
  returning `Unit` emits a `void` invocation; a `try`, a `when` and a safe call emit their branches for
  effect; a block ends in one of those — so binding the tail to the machine's result temp stored from an
  empty stack (`VerifyError: Operand stack underflow`). The tails that DO leave a value (an assignment, a
  `when` without `else`) are popped in statement position, so running EVERY `Unit` tail for effect and
  yielding `kotlin/Unit.INSTANCE` is uniformly correct — the same coercion a `Unit` value gets in
  argument position, and what the leaf form already did. A SAFE CALL counts: its `Unit?` is a `Unit` tail
  too (the value is discarded either way, and both arms of the null test leave the stack as they found
  it). Both suspend-lambda lowering forms apply that same semantic test: the general state-machine path
  and the leaf `invokeSuspend` path used when the body itself never suspends. The exception is a tail that
  SUSPENDS, which keeps its own shape so the flattener still sees it —
  for a CALL that means the call node itself (its arguments are evaluated unconditionally and hoist ahead
  of it, so a suspending argument is no reason to leave the void call unwrapped), for anything else
  anywhere inside (the suspension sits in control flow rewritten in place, and that machine still SKIPS
  rather than compiling: corpus `coroutines/varSpilling/kt75926`). This was the real cause of the corpus
  `coroutines/intLikeVarSpilling` failures, which the sub-int/array spill bail had been skipping by proxy
  (it keyed on a machine's leading `this` field, i.e. on the callee being a receiver lambda); that bail is
  removed and those cases now compile and run. Proven box-run for a void call, a function VALUE, a `try`,
  a safe call on both a present and a null receiver in the leaf and general-machine forms, a void tail
  whose argument suspends, and an inline-SPLICED tail
  (`tests/suspend_receiver_lambda_e2e.rs`, `tests/suspend_lambda_unit_tail_e2e.rs`).
- **A `suspend inline` callee inside a suspend lambda SKIPS (never miscompiles).** Its body must be
  spliced at the call site — the compiled method is not the one the source signature names — and the
  splicer does not reach into a state machine's states, so the machine would emit an ordinary call and
  fail at runtime with `NoSuchMethodError`. `Lower::body_calls_suspend_inline` walks calls in the body
  and reads each checker's exact provider-neutral `ResolvedCall`; it does not reselect by name or branch
  on local/module/classpath origin. Consequently an unrelated same-named declaration cannot suppress a
  valid ordinary call. The same exact target drives `Lower::ast_body_suspends`, so that ordinary call is
  not falsely promoted to a state machine either. This applies equally to convention syntax: the
  expression/statement target queries expose one selected capability pair across `resolved_calls`,
  `resolved_operator_calls`, `resolved_stmt_operator_calls`, and the specialized compound-assignment
  target. Without that shared query, a `suspend inline operator fun plus` promoted the lambda to a
  state machine but escaped this stricter gate because its target was not in `resolved_calls`; the
  generated state then emitted an unspliceable direct call. The selected suspend-inline target bails.
  Corpus `coroutines/kt15017.kt`, the collision regression in
  `tests/suspend_receiver_lambda_e2e.rs`, and the expression/statement convention regressions in
  `tests/coroutine_intrinsics_e2e.rs`.
- Integer overflow / wraparound semantics (Kotlin `Int` is 32-bit two's complement).
- Integer division/modulo by constants; `/` truncation toward zero; `%` sign.
- `Long` vs `Int` literal typing and promotion; `Double` arithmetic & NaN comparisons.
- String concat of mixed types (`Int + String`, `Boolean + String`) and evaluation order.
- `if`-as-expression typing (common supertype) and as-statement (Unit).
- Operator precedence/associativity vs Kotlin grammar (Pratt table must match).
- **Referential identity `===` / `!==`** (distinct from structural `==`): on reference operands it
  compiles to a JVM `if_acmpeq`/`if_acmpne` on the two object refs (`IrBinOp::RefEq`/`RefNe` — never
  `Intrinsics.areEqual`). On **primitive** operands Kotlin's `===` is just value `==`, so the backend
  remaps `RefEq`/`RefNe` → `Eq`/`Ne` and emits the ordinary numeric comparison (so `i === i` for `Int`/
  `Long`/`Double` works). `String` operands are **rejected** (the file skips): String identity depends on
  kotlinc's compile-time folding/interning of `const val`s (a computed const like `const val b = "1234$a"`
  folds to one interned literal, so `A.b === B.b`), which krusty does not model yet — it emits such a
  const as a runtime concatenation (a fresh object), so it can't reproduce String identity without
  miscompiling.
- **A MIXED reference/primitive `===`/`!==` boxes the primitive side and compares refs.** `a === 0`
  (`a: Comparable<Int>`) or `a === b` (`a: Any`, `b: Int`) is legal Kotlin — kotlinc accepts it with only
  a warning ("identity equality for arguments of types 'Any' and 'Int' can be unstable because of
  implicit boxing") and emits `aload_0; iload_1; Integer.valueOf; if_acmp*`. krusty matches: unless BOTH
  operands of a `RefEq`/`RefNe` are JVM scalars, the shared classifier
  (`emit_non_structural_compare_branch`, which serves value and branch position alike) takes the
  reference route and boxes the primitive operand in place with its wrapper's `valueOf` — so the two
  consumers cannot drift apart. The predicate is "not both scalars"
  (`identity_compares_refs`) rather than "either is a reference" because `is_reference()` is a
  LANGUAGE-level query that misses types which are still references on the JVM — `Ty::Unit`, whose value
  is the `kotlin/Unit.INSTANCE` singleton, and `Ty::Null`. Boxing is per operand, not once at the end: a
  `Long`/`Double` left operand occupies two stack words, so a boxed right operand could not be swapped
  past it. Only a pair of same-typed plain primitives is a value comparison and remaps to `Eq`/`Ne`;
  unlike primitive types (`Int === Long`, `Int === Char`, `Float === Double`) are rejected at the
  semantic boundary, matching kotlinc, before the emitter can select an incompatible JVM comparison
  family from one operand.
  Requiring BOTH operands to be references (the earlier condition) dropped a mixed pair into the numeric
  tail, where the int-vs-wide category was derived as "not `Long`/`Double`/`Float`" — which classifies
  every reference type as int-category. The result was an int branch on an object ref (`aload_0; ifne`,
  `aload_0; iload_1; if_icmpne`): a class file that is emitted successfully and fails only at class
  load, so **a compile-only assertion cannot catch it** — `VerifyError: Bad type on operand stack` /
  `Type 'java/lang/Object' … is not assignable to integer`. That categorization now goes through
  `numeric_cmp_int_category`, which asserts both operands are JVM scalars, so a future reference leaking
  into `emit_numeric_compare_branch` fails loudly instead of emitting unverifiable bytecode.
  `MixedRefPrimIdentity`/`MixedRefPrimIdentityGeneric` in `tests/feature_box_e2e.rs` (they RUN on a JVM;
  the expected results ride on the wrapper caches — `Integer`/`Long` cache -128..127, `Character`
  caches the ASCII range used by the fixture, `1000` is outside the integer cache, and
  `Double.valueOf` never caches).
- **A `Unit` operand of `===`/`!==` materializes `kotlin/Unit.INSTANCE`**, exactly as it already did for
  `==`/`!=` — the lowerer's `unit_value_after_effect` gate covers all four operators. A `Unit`-typed call
  leaves nothing on the stack, so without the `getstatic` each operand of `g() === g()` pushes NOTHING
  and the `if_acmp*` reads an empty stack (`VerifyError: Operand stack underflow`); the backend also saw
  a `Ty::Unit` that is neither `is_reference()` nor `is_jvm_scalar()`. Every call to a `Unit` function
  yields the same singleton, so identity holds. Byte-compatible with kotlinc. `UnitIdentity` in
  `tests/feature_box_e2e.rs`.
- **A primitive compared against the `null` literal boxes before `ifnull`/`ifnonnull`.** `x === null` for
  `x: Int` is legal Kotlin — kotlinc warns "condition is always 'false'" and folds the expression to
  `iconst_0`. krusty keeps the comparison and boxes the operand (`iload_0; Integer.valueOf; ifnonnull`),
  since the single-operand null branch tests a REFERENCE and `iload_0; ifnonnull` is the same
  int-under-a-reference-branch VerifyError. Same constant answer as kotlinc, verifiable, but not its
  folded form — krusty does not model the constant fold. (`x == null` on a primitive never reaches the
  backend: the front end rejects it, matching kotlinc's `==` typing.) `PrimitiveVsNullIdentity` in
  `tests/feature_box_e2e.rs`.
- **`===`/`!==` with an unsigned or `@JvmInline value class` operand is rejected** — kotlinc makes this a
  hard ERROR, not the implicit-boxing warning above ("identity equality for arguments of types 'Any' and
  'UInt' is prohibited"), because an inline class has no stable boxed identity. It applies to either
  side, to two operands of the same value class, and through nullability (`VC? === VC?`). krusty mirrors
  the error rather than boxing through `box-impl`, which would emit an `if_acmp*` for a program kotlinc
  refuses to compile. The value-class query is FEDERATED: module symbols publish their `value_field`
  through the same provider-neutral classifier shape as decoded dependency metadata, and the checker
  asks that common resolver once. A dependency inline class can otherwise reach the backend as its
  unboxed carrier, so an unrejected `a === b` silently compares two scalar carriers or boxes one as an
  unrelated JVM wrapper. No source/classpath branch is part of the identity policy.
  `referential_equality_on_a_value_class_operand` in `tests/resolve_parser_diag_coverage_e2e.rs`.
- `==` on `String` (Kotlin `==` = `.equals`, `===` = reference). Structural
  `==`/`!=` on reference operands compiles to `kotlin/jvm/internal/Intrinsics.areEqual(Object,Object)Z`
  — the exact helper kotlinc's JVM backend emits (`backend.jvm/.../intrinsics/Equals.kt`), so the
  bytecode matches (krusty previously used `java/util/Objects.equals`, which behaves identically but
  isn't byte-equal). Note: the Kotlin compiler exposes **no metadata** marking these intrinsics — the
  operation→helper mapping is a hardcoded registry in its backend (`IrIntrinsicMethods.kt`, keyed by
  built-in IR symbols), which krusty mirrors.
- **`Char` arithmetic**: `Char + Int` and `Char - Int` yield `Char`; `Char - Char` yields `Int` (the only
  `Char.plus`/`Char.minus` overloads — there is no `Char + Char`, `Char * …`, etc.). There is no numeric
  *promotion* between `Char` and `Int`, but both share the int stack slot, so the op runs on ints; a `Char`
  result is truncated back with `i2c` (Kotlin wraps mod 2^16, so `Char.MAX_VALUE + 1 == Char.MIN_VALUE`),
  matching kotlinc's `isub`/`iadd` + `i2c`. A `Char - Char` distance stays a plain `Int`.
- **A `Char` is a UTF-16 code UNIT, not a code point.** The surrogate range `D800..DFFF` therefore holds
  legal `Char` values (`Char.MIN_HIGH_SURROGATE == '\uD800'`, `Char.MAX_LOW_SURROGATE == '\uDFFF'`) even
  though those are not valid Unicode scalar values. `IrConst::Char` accordingly carries a raw `u16`, not a
  Rust `char`: routing the value through `char::from_u32` yields `None` on a lone surrogate, and inlining
  a classpath `Char` constant used to fold that `None` to NUL — `Char.MIN_HIGH_SURROGATE.code` printed
  `0` where kotlinc prints `55296`, a silent wrong value. The same rule holds one level up, in the AST:
  `Expr::CharLit` is a `u16` and `unquote_char` takes a `\uXXXX` escape verbatim, so a *source* literal
  `'\uD800'` keeps its code unit too (it used to fold to NUL by the same round-trip). A `char` that
  reaches either from a code POINT truncates with the JVM's own `i2c`, since a well-formed `Char`
  literal is always in the BMP. The code unit survives every encoding a `Char` constant reaches: a
  primary-constructor DEFAULT keeps it through both fill paths (the same-class path lowers the
  default's AST `Expr::CharLit`; a subclass's `: B()` fills the base's `super(…)` args from the
  file-independent `resolve::CtorDefaultValue::Char`, which is a `u16` for the same reason), and an
  ANNOTATION ARGUMENT is written as an `element_value` tagged `'C'` over a `CONSTANT_Integer` holding
  the raw code unit. Tests: `CharSurrogateConst`, `CharSurrogateLiteral`, `CharSurrogateCtorDefault`,
  `CharSurrogateWhen`, and `CharSurrogateAnnotationArg` in `tests/feature_box_e2e.rs`, plus
  `cross_file_super_ctor_char_defaults_keep_utf16_code_units` in
  `tests/cross_file_ctor_default_e2e.rs` for the sibling-file handoff.
- **A `Char` literal that is not exactly one UTF-16 code unit is REJECTED in the lexer.** The `i2c`
  truncation above is correct only because a *well-formed* literal is in the BMP, so the ill-formed ones
  have to be diagnosed rather than truncated: `const val E = '😀'` used to compile silently to
  `'\uF600'` — `unquote_char` truncates a code POINT to 16 bits, so U+1F600 landed on U+F600, not even on
  a surrogate half. A literal holds exactly one *element* — one BMP character, or one escape from
  Kotlin's set (`\n \t \r \b \\ \' \" \$` and `\uXXXX`; there is **no** `\0`, unlike C, and kotlinc
  rejects `'\0'`) — and never spans a line. kotlinc splits the failures by how the content STARTS, which
  krusty mirrors: `''` is `empty character literal`; content holding a raw CR or LF is `incorrect
  character literal` (a bare LF *is* one code unit, so the grammar bars it, not the count — and because
  the scan runs past a newline hunting the closing quote, this also covers an unterminated literal that
  found one further down); content beginning with a backslash must be exactly one valid escape or it is
  `unsupported escape sequence` (`'\0'`, `'\q'`, `'\u12'`, and the two-escape spelling of a surrogate
  pair, `'\uD83D\uDE00'`); anything else that is not one BMP character is `too many characters in a
  character literal` (`'ab'`, `'a\n'`, and a raw astral character — two code units, so it lands in the
  counting arm, not an encoding complaint). A LONE surrogate written as an escape (`'\uD83D'`) stays
  legal — it is one code unit — so the check never asks whether the result is well-formed UTF-16; a raw
  TAB stays legal too, since only CR and LF are excluded. The check belongs in the lexer because it needs
  nothing but the literal's own text, and sitting beside `unterminated character literal` it cannot be
  missed by a parser path that never reads the token; it therefore also covers a literal inside a string
  template, the `${'$'}` idiom's path. Two knowingly-unclosed edges: `'\'` is `unterminated character
  literal` where kotlinc says `unsupported escape sequence` (both reject), and string literals are
  unchanged — `unescape_chunk` still accepts `"\0"`, which kotlinc rejects. The sibling truncation in
  `ast_literal_const` (`Ty::Char => IrConst::Char(*v as u16)` for an `IntLit`) needs no diagnostic:
  `val c: Char = 128000` is already rejected upstream with the same `initializer type mismatch` kotlinc
  reports, so no source reaches it. Validation and decoding are one token-layer contract used by
  both lexer and parser; this avoids separate escape tables drifting while keeping the diagnostic at
  the lexer boundary. Tests: `tests/char_literal_diagnostics_e2e.rs`.
- **A `Char` constant folded into a string renders as the CHARACTER, not its code unit.** The constant
  string evaluator behind the `trimIndent`/`trimMargin` fold accepts a `Char` (`${'$'}` is the idiomatic
  way to write a literal `$` in a template), so it must spell the character out. Test:
  `ConstCharTemplateFold` in `tests/feature_box_e2e.rs`.
- **A `String` is a sequence of UTF-16 code UNITS**, the same rule as `Char` one level up. `"\uD800"` is
  a one-element string whose element is `Char.MIN_HIGH_SURROGATE`, and `"\uD83D\uDE00"` is U+1F600
  written as its two halves — neither has a Rust `String` spelling, because `char::from_u32` rejects a
  surrogate. Decoding each `\uXXXX` escape through `char` silently DROPPED both, so `"\uD83D\uDE00"`
  (an ordinary escaped emoji) compiled to `""` where kotlinc gives a 2-element string; and a `Char`
  template part with no scalar form made the whole `trimIndent`/`trimMargin` fold unrepresentable, so
  the file was rejected with "this construct is not yet supported by the IR backend" where kotlinc
  compiles it. String constants therefore carry `KtString` (`src/kt_string.rs`) — a `String` fast path
  that degrades to a `Vec<u16>` only for content with an unpaired surrogate — from `ast::Expr::StringLit`
  and `ast::TemplatePart::Str` through `File::const_string_value` and `IrConst::String` to the class
  file. The two representations are kept disjoint (`KtStringBuf::finish` re-tests the result, so a
  completed surrogate pair comes back out as text), which is what lets the constant pool keep deduping
  on value equality. `trimIndent`/`trimMargin` fold in code units too, matching how Kotlin measures an
  indent. Tests: `tests/utf16_string_constant_e2e.rs`, `kt_string::tests`.
  - Where that indent ENDS is `Char.isWhitespace()`, which on the JVM is
    `Character.isWhitespace(c) || Character.isSpaceChar(c)` — **not** Rust's `char::is_whitespace`
    (the Unicode `White_Space` property). Checked against JBR 21 over the whole BMP, the two sets
    differ in exactly five code points: Kotlin also counts the separators `U+001C..U+001F`, and does
    not count `U+0085` (NEL, a `Cc` control that is neither predicate). `U+00A0`/`U+2007`/`U+202F`
    agree — `Character.isWhitespace` alone excludes them, but `isSpaceChar` re-admits every
    `Zs`/`Zl`/`Zp` character. Test: `ir_lower::tests::unit_whitespace_matches_kotlins_predicate_not_rusts`.
  - `CONSTANT_Utf8` is **modified UTF-8**, whose units are UTF-16 code units: a supplementary character
    is written as its surrogate PAIR (two 3-byte sequences) and an unpaired surrogate encodes exactly
    the same way, so the class-file format carries these values unchanged (`modified_utf8_units`,
    `src/metadata/encoding.rs`). A JS string is likewise a code-unit sequence; the JS backend writes a
    lone surrogate as `\uXXXX`.
  - The `StringBuilder` template path appends a **one-code-unit** string constant as a `char`
    (kotlinc's form). "One character" must be counted in code units, not `char`s: a supplementary
    character is two units and does not fit a `Char`, so appending it that way would truncate it
    through `i2c`. It stays on the `append(String)` path.
  - The generic class reader decodes `CONSTANT_Utf8` to the same code-unit value and carries it through
    `ConstVal`/`LibConst`, so a separately compiled **classpath** `const val` preserves an unpaired
    surrogate too. Names and descriptors still require scalar text; an invalid name fails soft rather
    than leaking a replacement value into resolution. Likewise `@JvmName("…")` falls back to the
    declared name if given an unpaired surrogate — a JVM method name has no such spelling.
- Non-null reference parameters of a visible (non-`private`) function/method are guarded at entry with
  `kotlin/jvm/internal/Intrinsics.checkNotNullParameter(param, "name")`, in declaration order — matching
  kotlinc. Primitives, nullable params (`String?`), and generic type parameters (`T`) are not guarded.
  (krusty has no visibility model beyond `private`, and skips extension functions and constructors for
  now — minor byte-parity gaps, not correctness ones.)
- **Nullability is a first-class fact on `Ty`** (`Ty::Nullable(&Ty)`, `types.rs`), not faked as the
  boxed JVM wrapper. `Int?` is `Nullable(Int)` (a Kotlin-level type), and the boxing to a JVM reference
  (`Int?` → `Ljava/lang/Integer;`, `UInt?` → `Lkotlin/UInt;`, a nullable reference → its own descriptor)
  lives only in `Ty::descriptor()` — the backend boundary. `Ty::nullable` is idempotent (no `T??`) and
  collapses degenerate inputs (`Null?` = `Null`, `Error?` = `Error`); `Nothing?` is kept (it is the type
  of the `null` literal). Tests: `types::tests` (representation + descriptor boxing). The legacy
  wrapper-masquerade tables (`resolve::nullable_prim_wrapper`/`prim_of_wrapper`) are being retired onto
  this representation (consumer migration in progress).
- Boolean short-circuit evaluation (`&&`/`||`) side-effect order.
- Function call argument evaluation order; recursion.
- Shadowing of locals; `val` reassignment is an error.
- Empty file; file with only signatures; forward references between top-level functions.
- `data class`: `equals`/`hashCode`/`toString`/`componentN` are synthesized (in IR lowering, so all
  backends share them). `equals` compares field-wise with IEEE-aware `Double/Float.compare` and
  structural reference equality; `hashCode` is the `31*result + fieldHash` fold; `toString` is
  `Class(p1=v1, p2=v2)`. `copy(p = v)` is supported via the default-argument mechanism (below).
- **Default arguments.** A parameter's default *value* is backend-agnostic IR
  (`IrFile.fn_param_defaults`). A call that omits arguments is an ordinary call with holes —
  `IrExpr::MethodCall { args: Vec<Option<ExprId>> }`, `None` = omitted (mirrors Kotlin IR, where an
  `IrCall` argument may be null); there is no separate "defaulted call" node. The JVM backend realizes
  defaults exactly as kotlinc: a synthetic `name$default(self, params…, int mask, Object marker)` stub
  that, for each defaulted parameter, does `if ((mask & (1<<i)) != 0) param = <default>;` then tail-calls
  the real method; a call with holes passes the computed mask + null marker. Byte-identical to kotlinc
  for data-class `copy` and instance methods. **Mask bits are LOGICAL**: kotlinc numbers them over
  the DECLARED value parameters, so an EXTENSION's receiver — physically the leading parameter of
  the static realization — does not shift them (`fun Host.tag(name, port = 9)` → `port` is bit 2
  = 1<<1, decoded from kotlinc 2.4.0; krusty once numbered physically, bit 4, so a kotlinc-convention
  caller's omitted `port` silently kept the zero placeholder). The stub emitters slice the receiver
  prefix off the registered defaults and offset the parameter slots; member-`$default` call sites
  subtract the member-extension receiver from the bit index the same way. Interface defaults use the
  mode-selected interface or `$DefaultImpls` realization. More than 31 parameters (kotlinc's
  multi-`int` mask) remains unmodeled and is skipped, never miscompiled. **Stub emission is decoupled
  from same-module call-site filling**: a top-level EXTENSION with a NON-CONSTANT default
  (`fun Icon.toSwingIcon(scale: IconScale = IconScale.Default)`) registers its lowered defaults
  STUB-ONLY (`FnParamInfo::stub_only`) — kotlinc emits `name$default` for it, and suppressing the
  stub is a silent ABI gap (an omitting cross-module/Java caller gets `NoSuchMethodError`; found
  byte-verifying intellij's icons-api `SwingIconKt`, krusty 3 methods vs kotlinc 5). A SAME-MODULE
  omitted-arg call to such an extension still inlines only checker-recorded constant defaults and
  otherwise bails (skip, never miscompile) — module calls are deliberately not routed through the
  stub (tests: `extension_default_stub_e2e`).
- `-jvm-default` (interface members with bodies): kotlinc offers three JVM realizations of the same
  Kotlin source, and the flag changes the CLASS SET, not just method bodies. Measured against
  kotlinc 2.4.10 on an interface with a default getter, a default method, a defaulted parameter and
  an abstract method:
  * `enable` (kotlinc's own default since 2.2; legacy `-Xjvm-default=all-compatibility`) — default
    methods on the interface plus synthetic `access$<name>$jd` bridges; an `<Iface>$DefaultImpls`
    holder whose statics forward to those bridges; forwarder overrides on every implementing class;
    `@Metadata` `jvmClassFlags` (`Class` extension field 104) = 3.
  * `no-compatibility` (legacy `-Xjvm-default=all`, what intellij-community builds with) — default
    methods only. NO `$DefaultImpls` class anywhere, no class forwarders, `jvmClassFlags` = 1, and a
    compiler-version requirement for 1.4.0 in the class metadata.
  * `disable` — every interface member abstract, the real bodies on `$DefaultImpls` as statics taking
    the receiver as parameter 0, implementing classes forwarding with `invokestatic`, and no
    `jvmClassFlags` field at all.

  krusty emits and accepts all three modes. Under `disable`, holder methods, class forwarders,
  `super` calls, properties, and default-argument calls use provider-recorded realizations across
  source files and module boundaries; a consumer's own mode never reinterprets a dependency.
  `$DefaultImpls` holder bytes are differential-tested exactly against kotlinc, including their
  generic receiver signatures, parameter annotations, local-variable slots, `InnerClasses`, and
  synthetic Kotlin metadata.

  `enable` (krusty's default) emits the full measured compatibility surface: a `public static
  synthetic access$<name>$jd` bridge on the interface per non-private body (an `invokespecial` on
  the interface's own default method; `LineNumberTable` = one entry at the invoke pc on the
  interface's declaration line), a `$DefaultImpls` holder whose statics FORWARD to those bridges
  (each carrying the `Deprecated` attribute + a runtime-visible `@java.lang.Deprecated`, the
  re-emitted `checkNotNullParameter` guards with the line entry at the post-guard pc, the promoted
  generic signature, `@NotNull`/`@Nullable` annotations, and `$this`-first locals), a `$default`
  holder copy that is a thin synthetic forward to the interface's own stub, and an `ACC_BRIDGE`
  forwarder override on every implementing class — an `invokespecial` that must NAME a direct
  superinterface (the first declared one through which the winning declaration is inherited;
  measured on the diamond). A sub-interface REPUBLISHES the surface for every inherited default it
  does not redeclare, even when it declares nothing itself; a member inherited from a
  `disable`-compiled dependency gets a holder forward straight to that dependency's holder (behind
  a `checkcast`, without `@Deprecated` or an `access$…$jd` bridge), exactly as measured. Kotlin-ness
  gates the surface (`LibraryType::is_kotlin`): a JAVA interface's default method never gets a
  forwarder. The `enable` holder bytes are differential-tested exactly against kotlinc like the
  `disable` ones, and a kotlinc-compiled `disable` downstream module is compiled AND RUN against a
  krusty-built `enable` interface — the shape whose forwarders link `invokestatic` against the
  holder statics the `@Metadata` `jvmClassFlags` = 3 advertises. An interface property accessor is
  described ONLY by its `Property` metadata record — recording the accessor as a `Function` too
  made every kotlinc consumer report "inherited platform declarations clash" on each implementer;
  the accessor match is DESCRIPTOR-aware, so `fun getX(): Int` beside `val x: String` keeps its
  `Function` record. Forwarder suppression against a class's own property accessors is keyed the
  same way, on the accessors the class actually EMITS: a `val` never stands in for an inherited
  `setX(I)V` (dropping that forwarder left the class abstract), and a same-name accessor with a
  different return coexists with its forwarder, as kotlinc emits both.
  A `suspend` member's forwarders and republished surface use its CPS shape — a trailing
  `Continuation` parameter (`$completion`, `@NotNull`) and a `@Nullable Object` return — never the
  declared signature: a forwarder built from the semantic `s(): Int` names an `s()I` the interface
  does not have, and the class calls a `NoSuchMethodError` into existence (this also fixed the
  pre-existing `disable`-mode forwarder shape for a suspend default member).
  Known remaining gaps, each measured: member ORDER diverges when a property precedes a function
  (krusty emits accessors after methods, in every mode); the interface's own `$default` stub does
  not yet carry kotlinc's super-call guard; a REPUBLISHED (inherited) holder forward does not
  reconstruct the declaring classifier's generic signature (and a suspend forwarder omits kotlinc's
  `Signature` attribute); a suspend default method lacks kotlinc's `s$suspendImpl` static
  indirection on the interface (a pre-existing suspend-lowering divergence); and interface member
  metadata does not yet carry kotlinc's open-modality and accessor flag bytes.

  The box corpus compiles each test under the mode its
  `// JVM_DEFAULT_MODE:` directive pins; every recognized mode runs, including multi-module
  `disable`. Tests: `tests/jvm_default_mode_e2e.rs` (differential class sets, public method
  realization and holder bytes vs kotlinc, emitted `jvmClassFlags`, behavior parity, and cross-module
  consumption) and the `-jvm-default` parsing tests in `crates/krusty-cli/src/cli.rs`.
- `-Xconsistent-data-class-copy-visibility` (language feature
  `DataClassCopyRespectsConstructorVisibility`; also reachable as `-XXLanguage:+…`): a data class's
  synthesized `copy`/`copy$default` take the PRIMARY CONSTRUCTOR's visibility instead of being
  unconditionally public. Measured against kotlinc 2.4.10 on
  `data class D private constructor(val s: String, val n: Int)`: `copy` becomes
  `ACC_PRIVATE|ACC_FINAL`, loses its `@NotNull` nullability annotations (kotlinc annotates no
  private method), and drops its `Intrinsics.checkNotNullParameter` entry guards (kotlinc guards
  only functions reachable from Java — the private body starts directly at `new`); `copy$default`
  becomes package-private `ACC_STATIC|ACC_SYNTHETIC` and dispatches to `copy` via `invokespecial`
  (a private member is non-virtual); the `@Metadata` copy `Function.flags` visibility bits go
  private (0xC6 → 0xC2, byte-equal d1 verified); nothing else changes. krusty routes the flag
  through `LangFeatures` onto the `File`, and the lowering marks the synthesized `copy` in the same
  `private_methods`/`internal_methods` sets a declared member uses (and gates its param guards the
  same way `param_checks_for` gates a declared private member's), so the emitter's existing
  visibility machinery produces all of the above. An INTERNAL primary ctor records internal copy
  visibility in `@Metadata` while the JVM method stays public and UNMANGLED — krusty's systemic
  internal-member convention (no `$module` mangling anywhere yet); kotlinc's `copy$<module>` byte
  shape is deferred until internal mangling lands module-wide. A PROTECTED primary ctor currently
  falls back to a public `copy` (kotlinc emits a protected `copy` with a public `copy$default`) —
  the IR visibility sets model neither, a silent divergence on a rare shape, like a declared
  `protected fun`. krusty also does not yet ENFORCE the copy's visibility at call sites: an
  out-of-scope `d.copy()` still compiles where kotlinc reports an error under the flag. The
  per-class `kotlin.ConsistentCopyVisibility`/`kotlin.ExposedCopyVisibility` annotations are
  unhandled.
  Tests: `tests/consistent_copy_visibility_e2e.rs` (normalized `copy`/`copy$default` javap sections
  differential vs kotlinc with and without the flag, byte-level toggling isolation, internal-ctor
  pin) plus the flag-modeling tests in `crates/krusty-cli/src/cli.rs` and `src/features.rs`, and the
  Bazel worker acceptance test in `crates/krusty-cli/src/worker.rs`.
  realization and holder bytes vs kotlinc for `disable` AND `enable`, emitted `jvmClassFlags`,
  behavior parity, sub-interface republication, and cross-module consumption in both directions —
  krusty consuming each dependency mode, and kotlinc consuming a krusty `enable` jar) and the
  `-jvm-default` parsing tests in `crates/krusty-cli/src/cli.rs`.
- `-Xno-param-assertions` / `-Xno-call-assertions`: the two null-check families kotlinc emits, and
  which a build can turn off. Measured against kotlinc 2.4.10:
  * `-Xno-param-assertions` removes every `Intrinsics.checkNotNullParameter` — the guard at the entry
    of any function reachable from Java, including constructors that store a property. krusty honors
    it in three places, because a guard has three origins and each must agree or the class is
    malformed: the lowered guards are cleared from the IR before emission
    (`jvm::ir_emit::strip_param_assertions`); a property SETTER's `<set-?>` guard, derived at emission
    from the property type, is gated by `EmitOptions::param_assertions`; and the same option stops
    `ClassWriter` seeding the pool with the guard's `Methodref` and `String` constants. Every
    debug-table offset measured PAST a guard is gated too — the primary constructor's
    `LineNumberTable` start pc counted guards that were no longer emitted, which put the entry past
    the end of the method and made the JVM reject the class outright (`ClassFormatError: Invalid pc
    in LineNumberTable`).
  * `-Xno-call-assertions` removes `Intrinsics.checkNotNullExpressionValue` on platform-typed values
    returned by a CLASSPATH Java class — but not the one kotlinc emits for `String.substring`, a
    mapped builtin, which the flag leaves in place. krusty emits those guards (see the
    platform-narrowing entry below) and honors the flag by rewriting each guard node into its operand
    before emission (`jvm::ir_emit::strip_call_assertions`), which leaves `x!!` — a SOURCE assertion
    sharing the same IR node — untouched. The Bazel worker forwards `--x_no_call_assertions` to the
    compiler rather than reporting it inert.

  Both are set per module by intellij-community (`build/compiler-options.bzl`). Tests:
  `tests/no_assertions_flags_e2e.rs`.
- `-Xlambdas` / `-Xsam-conversions`: how a lambda and a SAM conversion are realized. krusty emits
  `indy` — an `invokedynamic` call site bound through `LambdaMetafactory.metafactory` — always, which
  is kotlinc's own default since 2.0 and what intellij-community builds with. Verified against
  kotlinc 2.4.10 for a Kotlin function type, a `fun interface`, and a Java SAM (`Runnable`): the same
  class set (no synthetic lambda class), one `invokedynamic` per lambda, and an identical
  `BootstrapMethods` table — modulo constant-pool indices, which differ because the two pools differ
  in size — down to the synthetic implementation-method names (`box$lambda$0`…). The `class` strategy
  (kotlinc's pre-2.0 realization, selected per module by 40 intellij-community `BUILD.bazel` files)
  is emitted too: each lambda becomes its own class extending `kotlin/jvm/internal/Lambda` — a
  non-capturing one a static `INSTANCE` singleton, a capturing one constructed per evaluation. The
  two flags are independent; each selects only its own closure kind. **Synthetic class names match
  kotlinc's declaration-derived scheme**, taken from the lambda's stable lexical origin recorded at
  lowering (`IrLambdaOrigin`), never from a generated method spelling or a value-table scan: a lambda
  initializing a binding is `Owner$fn$binding$1`; in a CLASS-INITIALIZATION context (a property
  initializer or an `init` block, which lower with an EMPTY enclosing-function scope) the name
  carries no function segment — `C$prop$1`, `C$local$1`, bare `C$1` for an unbound init-block lambda
  (empty name segments are dropped, never printed as `C$$1`). Ordinals are counted per RENDERED
  prefix, not per raw `(enclosing, binding)` context: property `x` (`("x", None)`) and init-local
  `x` (`("", Some("x"))`) both render `C$x$…`, and kotlinc numbers them as one sequence — `C$x$1`,
  `C$x$2` — where a raw-context counter would number both `$1` and one class file would silently
  overwrite the other (`tests/class_lambda_e2e.rs`, the `Collide` fixture, pinned at runtime). A
  DELEGATED property's initializer is property-scoped the same way (`val z by lazy { … }` →
  `C$z$…`, impl `z$lambda$0`); one recorded gap: kotlinc numbers that delegate lambda `C$z$2` where
  krusty emits `C$z$1` — kotlinc's delegate ordinal counts a slot krusty does not model
  (`tests/class_lambda_e2e.rs::delegated_property_lambda_takes_the_property_name` asserts krusty's
  deterministic set). The synthetic impl METHOD prefix in
  that context is a different name: the PROPERTY name for a property initializer (`h$lambda$0`, and
  same-named declarations share one sequence — `val member` + `fun member` → `member$lambda$0/1`)
  and kotlinc's `_init_` for an `init` block (`_init_$lambda$0`), never whichever function the
  lowerer visited last (`tests/lambda_e2e.rs::class_init_lambda_impl_methods_use_declaration_prefixes`). A class property initializer is lowered
  into EVERY constructor but keeps one source identity, so a multi-constructor class still emits ONE
  lambda class (dedup by `(impl_owner, source expression)`), exactly as kotlinc does. Because
  `invokedynamic` requires class-file version 51 or newer, a real indy call site under
  `-jvm-target 1.6` fails the compile without emitting artifacts; fully spliced inline lambdas remain
  valid because they emit no call site, and the check keys on emitted indy sites, so
  `-Xlambdas=class -Xsam-conversions=class -jvm-target 1.6` compiles (that pairing is the point of
  the mode) and stamps major version 50. Tests: `tests/indy_lambda_parity_e2e.rs`,
  `tests/class_lambda_e2e.rs`.
- `enum class`: compiled as a `final` class extending `java/lang/Enum` with a `public static final`
  constant per entry, a synthetic `$VALUES` array, a private `(String name, int ordinal, …userArgs)`
  constructor calling `super(name, ordinal)`, a `<clinit>` that constructs entries in declaration
  order, and synthetic `values()`/`valueOf(String)`. `e.ordinal`/`e.name` are `Enum.ordinal()`/
  `name()`; entry equality is reference identity (`==`). Entry constructor args are constant
  expressions evaluated in `<clinit>` (branchy args are spilled to `<clinit>` temps).
- **Enum entries with a body / abstract enum members**: an `abstract fun`/bodied entry makes the enum
  `ACC_ABSTRACT` (not `final`); each entry with a body (`ENTRY { override fun m() = … }`) is emitted
  as a synthesized package-private `final` subclass `Enum$ENTRY extends Enum` whose constructor
  `(String, int, …userArgs)V` delegates to the enum's constructor (made package-private so the
  subclass can call it) and whose overrides are lowered with the enum's `this`/field scope (so an
  override may read a constructor `val` as a `getfield` on the enum). The `<clinit>` constructs such
  an entry as `new Enum$ENTRY(name, ordinal, …)`. An abstract enum member requires every entry to
  override it (else the file is skipped, never miscompiled); property overrides in an entry body
  (`override val`) are not yet modeled — skipped.
- **Every enum classifier has the synthetic `entries: EnumEntries<E>` property.** Resolution selects
  the enum by semantic type identity, including nested and cross-file classifiers, then carries the
  exact zero-argument static accessor advertised by that symbol provider into lowering. Source/module
  and dependency shapes therefore share one target handoff; lowering never reconstructs a call from
  the declaration origin. If a provider exposes the enum kind but no direct accessor realization, the
  valid property is typed but rejected before emission with a stable boundary until an alternative
  cached-mapping realization is implemented.
- Explicit builtin operator-methods on numeric primitives: `a.plus(b)` ≡ `a + b` (same promotion);
  `a.compareTo(b)` uses IEEE total order (`{Integer,Long,Float,Double}.compare`, so
  `0f.compareTo(-0f) == 1`, `Double.NaN.compareTo(x) == 1`). Kotlin routes the *infix* form
  `a rem b` to a user `operator`/`infix` extension but the *dot* form `a.rem(b)` to the builtin;
  the parser records infix-call source form so resolver/lowering keep that distinction
  (`resolver_regression_e2e::primitive_builtin_infix_extension_source_form_matters`,
  box `infixFunctionOverBuiltinMember.kt`). `mod`/`rangeTo`/`inc`/`dec` unsupported.
  The bitwise/shift members on `Int`/`Long` (`a.and(b)`/`a or b`, `a.shl(n)`/`a shr n`/`a ushr n`,
  `a.xor(b)`) and Boolean bitwise members (`b.and(c)`/`or`/`xor`) lower to the corresponding
  `iand`/`ior`/`ixor`/`ishl`/… intrinsic; shifts take an `Int` count, the others the receiver's own
  type. `compareTo` and the arithmetic/bitwise/shift members all share `lower_prim_op_method`.
  A safe call uses that same operation on the non-null receiver value. Krusty collapses an unnecessary
  safe call on a statically non-null primitive to the qualified operation (and its non-null result),
  while a genuinely nullable primitive receiver is unboxed through the ordinary argument-coercion
  path and its result is boxed for the nullable merge. `inv()` (zero-arg) stays a dedicated arm.
  (`tests/safe_call_primitive_e2e.rs`.)
- Safe call `a?.b` / `a?.m(args)`: evaluates the receiver once into a temp, then yields the member
  access when the temp is non-`null`, else `null` — i.e. `{ val t = a; if (t != null) t.b else null }`.
  Inside the non-null arm the receiver expression is substituted with the temp and re-enters the same
  qualified-access lowering used by `.`, so source/module members and extensions, classpath members
  and extensions, primitive intrinsics, array operations, and `kotlin/Any` virtuals do not acquire
  separate safe-call dispatch tables. Resolution likewise normalizes the receiver to its non-null
  semantic type before selecting those targets. An applicable member still wins over an extension,
  including an inherited universal member such as `Any.toString()` when the same-named extension is
  declared on the receiver's superclass or interface; an inapplicable same-named overload does not
  veto the applicable member. Whether a primitive arrived boxed from `Int?` or unboxed from `Int` is
  a lowering representation, not a callable origin. A statically non-null scalar receiver delegates
  directly to the complete qualified operation; a nullable receiver's merge boxes primitive member
  results so both branches are references, and composes with Elvis (`a?.m() ?: d`).
  Primitive conversions, unmodelled builtin methods (`inc`/`dec`/`mod`/`rangeTo`), erased type-parameter
  receivers, local functions, and function-object `toString`/`hashCode` remain rejected rather than
  being rebound to a different origin or emitted with the wrong representation.
  (`tests/safe_call_e2e.rs`, `tests/safe_call_primitive_e2e.rs`,
  `tests/safe_call_any_member_e2e.rs`.)
- **Safe call whose scope block diverges — `x?.let { return … }` / `x?.run { throw … }` / `x?.also { … }`
  / `x?.apply { … }`.** A scope function whose lambda body is a non-local `return` (or `throw`) has block
  value type `Nothing`, so the whole safe call is `Nothing?` — `null` when the receiver is null, else
  control leaves via the return/throw and never comes back. For the body-returning scope fns (`let`/`run`)
  the checker types the safe call as `Ty::nullable(Ty::Nothing)` (parallel to the `Unit?` case for a `Unit`
  block), a reference type so it is not rejected as a "non-reference result". The receiver-returning scope
  fns (`also`/`apply`) keep the receiver's (reference) type, so their divergence is invisible to the result
  type — the lowerer detects it from the block-body type instead. In BOTH cases the lowerer must not model
  the non-null arm as a value-producing `when` branch: a diverging arm yields no value, and merging it with
  the `null` arm leaves a `top` on the stack (`VerifyError`). Instead it emits the divergent member as a
  guarded statement — `{ val t = a; if (t != null) { <member> }; null }` — and yields `null` unconditionally
  (only observed when `t` was null); the `also`/`apply` inliner additionally drops its unreachable
  "read the receiver back" tail. This is keyed on the `Nothing` result/block type, not on the scope-function
  name (the divergence guard is gated only on the call being one of the four scope fns, which run the body
  exactly once — a collection HOF like `forEach` may run it zero times and is excluded), and reproduces with
  a plain nullable receiver (no higher-order call involved). The value form (`val r = c?.let { return … }`)
  types `r` as `Nothing?`, which flows into any reference target. (Only the SAFE-call `?.` form is handled;
  a non-safe qualified `b.also { return … }` remains unsupported.) (`tests/qq1_safecall_diverging_scope_block_e2e.rs`).
- **A receiver that can only be `null` — `null?.m()`, `Nothing?`, `Nothing`.** `Nothing` has no non-null
  value, so a `?.` on a receiver typed `Null`, `Nothing?`, or `Nothing` never invokes the member: the whole
  safe call is `null`. The lowerer folds it to `{ evaluate receiver; null }` — the receiver still runs for
  its side effects, and a *diverging* receiver (`boom()?.toString()`) simply terminates there. This is one
  rule for all three receiver types rather than a special case for the `null` literal; a `Nothing?` receiver
  has no class internal to look a member up on, so no other lowering could serve it.
  The fold bypasses member resolution entirely, which is sound only because the CHECKER reports an
  unresolved member behind `?.` (next bullet). (`tests/safe_call_unresolved_member_e2e.rs`.)
- **An unresolved member behind `?.` is a checker diagnostic, exactly as for the qualified form.**
  `s?.thisDoesNotExist()` reports `unresolved reference 'thisDoesNotExist'.` at the member-name span,
  matching kotlinc. Previously only the PROPERTY spelling (`s?.thisDoesNotExist`) reported — it routes
  through `check_member` — while the CALL spelling exhausted every callable origin and returned a silent
  `Ty::Error`. The consequences were that the backend bail ("this construct is not yet supported by the IR
  backend") did frontend duty for a `String?` receiver, and that `null?.thisDoesNotExist()` compiled clean,
  because the always-null fold returns before any backend check. The report is guarded by a diagnostic
  checkpoint so an origin that already reported (a rejected classpath overload, an unmappable labelled call)
  is not reported twice, and by an EXISTENCE probe (`member_name_exists_on`, shared with the qualified
  arm's nullable-receiver check) so a member that exists but that this arm merely cannot SELECT stays a
  silent `Ty::Error`. That distinction is the whole point: `d?.toInt()`, `b?.not()`, `f?.invoke(1)`, and an
  arity mismatch like `s?.let(1)` are all real Kotlin krusty rejects in the BACKEND, and calling them
  "unresolved reference" would tell the user their program is wrong. Only a name that exists nowhere on the
  receiver is a typo. The classpath-less String fallback stores name, parameter shapes, and return in one
  semantic table: selection matches a complete shape, while the existence guard checks the name alone, so
  an overload mismatch cannot be mislabeled as a missing member. The same name-only rule covers universal
  `Any` callables (`toString`/`hashCode`/`equals`) on every receiver and function-value `invoke`; argument
  count and types never participate in the typo predicate.
  A second consequence of no longer being silent: the qualified and safe-call arms must agree about what
  EXISTS, so the classpath-less `String` table (`substring`/`indexOf`/`trimIndent`/`trimMargin`,
  consulted only when no stdlib is on the classpath) is shared by both. Those names are stdlib EXTENSIONS
  on `kotlin.String` rather than members of it, so both call forms consult the table LAST — after the
  ordinary source/classpath extension ladder — and a user's same-named extension wins. The lowerer's
  constant fold for literal `trimIndent`/`trimMargin` follows the same rule: it runs only when the checker
  recorded no callable target, never merely because the member name matches.
  Known gap: the checkpoint is taken after the receiver but before the arguments, so an argument that
  itself reports (`s?.nope(undefinedVar)`) suppresses the member report — the program is still rejected,
  with one diagnostic instead of two. (`tests/safe_call_unresolved_member_e2e.rs`.)
- **An unresolved qualified type names its first failed segment.** Binding records that segment once;
  diagnostics consume the recorded result without repeating lookup. A qualified `TypeRef` currently
  carries one span for the complete spelling, so its diagnostic range covers the full reference.
- Lambdas `{ a, b -> … }`: a function type `(A,…) -> R` is the JVM interface
  `kotlin/jvm/functions/Function{arity}`. A non-capturing lambda compiles to `invokedynamic` bound by
  `LambdaMetafactory.metafactory` to a synthesized `private static` method `<enclosing>$lambda$<n>`
  holding the body (with the lambda's real parameter types). The `implMethod` is primitive-specialized
  (`box$lambda$0(I)I`) while the `instantiatedMethodType` is boxed (`(Integer)Integer`), so the
  metafactory inserts the box/unbox adapter — matching kotlinc 2.x. Calling a function value `f(args)`
  goes through `FunctionN.invoke` (`(Object…)Object`): arguments are boxed, the result cast/unboxed to
  the return type. Only non-capturing lambdas returning a concrete non-`Unit` type, passed to a
  non-generic function, are supported; capturing lambdas, `Unit`/`Nothing` lambdas (need the
  `kotlin/Unit` singleton), lambdas inside class methods, and generic/suspend consumers are skipped
  (`tests/lambda_e2e.rs`, `tests/indy_infra_e2e.rs`).
- **Implicit `it` in an untyped lambda is lexical, not textual.** When no expected function type has
  established the lambda's parameters, a parameterless lambda synthesizes `it` only if its body uses
  that name and no enclosing scope already binds it. Thus `outer?.let { sink.emit { "$it" } }` passes a
  `Function0` to `emit` and captures the outer `it`; likewise, `{ it }` captures a local named `it`.
  Typed lambdas still receive and shadow with their expected `Function1` parameter. Overload probing and
  fallback lambda typing share this decision so they cannot infer different arities
  (`tests/classpath_object_member_import_e2e.rs`,
  `tests/nested_lambda_capture_e2e.rs::untyped_lambda_captures_local_named_it`).
- **Mutable capture**: a local `var` written by a non-inlined lambda (a closure) needs a shared mutable
  cell so writes are visible to the enclosing scope and vice versa. The lowerer computes this per body by
  checking whether a lambda captures a `var` from an outer local scope; the JVM realization currently
  uses a `kotlin/jvm/internal/Ref$XxxRef` holder. An inlined scope function (`let`/`also`/`run`/`apply`)
  needs no shared cell because its body is inlined, and a closure that writes a *field* (capturing
  `this`) is still skipped.
- Classes with **no primary constructor** (`class A { constructor(…) { … } }`): every constructor is a
  secondary `<init>`. A constructor delegating to `super(…)` (or implicitly, to a no-arg base/`Object`)
  runs the field initializers + `init {}` blocks (source order) before its own body; one delegating to a
  sibling `this(…)` runs only its body (the init steps run in the reached `super`-constructor). The
  parenless base class (`class A : B { constructor(): super() }`) is recovered semantically after
  parsing: the all-files bootstrap classifies module declarations and the composite symbol source
  classifies both module and library types, so same-file, other-file, and classpath bases produce the
  same superclass shape. **Field-initializer default-value elision:** kotlinc omits a field initializer
  that stores the field's JVM default (`0`/`false`/`null`/`'\0'`, incl. `0.toByte()`), so a value a base
  constructor's virtual call already wrote survives; krusty does the same (test
  `secondary_ctor_noprimary_e2e`, corpus `fieldInitializerOptimization`). The delegation `<init>`
  *target signature* is read live from the (post-`value_classes`-pass) class at emit time, so the lowerer
  needs no value-class knowledge and a value-class `super(…)` argument erases correctly. A secondary
  constructor with lowerable defaults emits and calls the synthetic `DefaultConstructorMarker` overload;
  when that ABI cannot be emitted, the file is skipped rather than calling a nonexistent target.
  Ambiguous `this(…)`/`super(…)` targets and delegation cycles are diagnosed. Tests:
  `tests/secondary_ctor_this_sibling_e2e.rs` and `tests/super_to_base_secondary_ctor_e2e.rs`.
- Constructor references `::A`: lowered like a lambda `{ args -> A(args) }` — a synthesized static
  impl `(ctor params) -> new A(params)` wrapped in the same `invokedynamic`/`LambdaMetafactory`
  closure. Only the simple primary-constructor positional case (the reference's arity matches the
  constructor's field params) is modeled; defaulted/secondary constructors are skipped.
- Method references `obj::m` (bound) and `Type::m` (unbound): a synthesized static impl
  `(receiver, args…) -> receiver.m(args)` — bound captures the receiver into the closure (so its
  arity is the method's), unbound takes the receiver as the first parameter. Only user-class methods
  (resolvable in the IR class table) and non-`Unit`/`Nothing` returns are modeled.
- Unbound top-level function references `::foo`: same `invokedynamic`/`LambdaMetafactory` lowering as a
  lambda, but the impl method handle points directly at the referenced function (no synthesized body).
  Exception: a `Unit`-returning `::foo` gets a synthesized wrapper `(params) -> { foo(params); Unit }`
  so the SAM's `invoke` yields the `kotlin/Unit` singleton (a direct `void` handle would adapt to
  `null`, breaking a `FunctionN` consumer that expects `Unit`).
  kotlinc instead emits a `kotlin/jvm/internal/FunctionReferenceImpl` subclass carrying reflection
  metadata, but that class is synthetic and not part of the facade's ABI, so public signatures and the
  round-trip result match. A function type lowers to the backend-neutral `IrType::Function`; the **JVM
  backend** maps it to `kotlin/jvm/functions/FunctionN` and enforces the JVM-only fixed-arity limit
  (`Function0..22`) — higher arities, and bound/object/constructor references, are skipped
  (`tests/callable_ref_e2e.rs`).
- Receiver (extension) function types `Recv.() -> R` / `Recv.(A) -> R`: parsed by **folding the
  receiver in as the first `FunctionN` parameter** — `Recv.() -> R` ≡ `Function1<Recv, R>`,
  `Recv.(A) -> R` ≡ `Function2<Recv, A, R>` — exactly how Kotlin lowers an extension-function type to
  `FunctionN`, so the rest of the pipeline sees a plain `(Recv, …) -> R`. This is a **parse**-level
  decision (`src/parser.rs`, `receiver_function_type_param` test); a call site that invokes such a
  parameter with an *implicit* receiver (the builder pattern `instructions()` / `recv.block()`) needs
  receiver-rebinding the checker does not yet model, so those still skip cleanly rather than
  miscompile (0-FAIL preserved).
- Labeled loops `l@ for/while/do { … break@l / continue@l }`: the `l@` label is parsed onto the loop
  (AST + IR carry an `Option<String>` label); the emitter's loop stack keeps each loop's source label, so
  a `break@l`/`continue@l` targets the nearest enclosing loop carrying `l` (an unlabeled `break`/`continue`
  still targets the innermost). Works across all loop forms — counted `for`, collection `for-each`,
  `while`, `do…while` (`LabeledLoops` in `tests/feature_box_e2e.rs`).
- Not-null assertion `x!!`: yields `x`, throwing a `NullPointerException` if it is null. Compiled (on a
  reference operand) as `dup` + `kotlin/jvm/internal/Intrinsics.checkNotNull(Object)V` — the value
  stays on the stack and the duplicate is consumed by the check, matching kotlinc. On a non-null
  primitive operand it is a no-op (`tests/not_null_assert_e2e.rs`).
- **A PLATFORM value narrowed to a declared non-null type is guarded where it enters.** A Java value
  arrives as `T!` (`Ty::PlatformNullable`), usable as both `T` and `T?`. Where the source commits it to
  a declared non-null type, kotlinc emits `dup` + `ldc "<expression>"` +
  `Intrinsics.checkNotNullExpressionValue(Object, String)V` — the same yields-or-throws shape as `x!!`,
  with the checked expression named in the failure (`getenv(...) must not be null`). Without it the
  declaration and its `@NotNull` annotation promise something the bytes do not enforce: the null is
  stored into an `@NotNull` field and surfaces later, in code that trusted the declaration.

  Measured against kotlinc 2.4.10, the GUARDED positions are: a property with an explicit type (member
  or top-level — the top-level one runs in `<clinit>`), a local with an explicit type, a value
  argument (including the parameter of a non-null-typed lambda, which is an `invoke` argument), a
  `return` / expression body, and an assignment to a non-null target. kotlinc does NOT guard: an
  INFERRED local (`val x = getenv(...)` stays `T!`), a nullable target (`String?`, elvis, `?.`), a
  `when` subject, a string-template interpolation, or the receiver of a Java member call (`getenv(..).length`
  NPEs on its own). Guards are recorded by the checker at the narrowing positions
  (`TypeInfo::platform_narrowings`) and realized by lowering, so both halves are one rule rather than a
  per-position emitter.

  The failure message is derived from the checked expression, as kotlinc's is: a call of any linkage
  renders as `<jvm-name>(...)` (a property read of a Java getter is its ACCESSOR, `getName(...)`), and a
  read resolved to a physical field renders as the bare field name. Where the callee's erased result
  carries a `checkcast` (a generic Java return), the guard goes UNDER the cast — kotlinc checks what the
  call produced, then casts the checked value.

  A CONDITIONAL value is checked per branch, not once at the merge: a DECLARED type propagates into an
  `if`/`when`/elvis, so kotlinc guards inside the branch that produced the platform value, before the
  merge (nested conditionals included). A value ARGUMENT's expected type does not propagate that way —
  kotlinc checks the merged value instead — so an argument whose value is a conditional takes the
  message-less form below. A branch that is a source BLOCK (`if (c) { …; javaCall() }`) is likewise not
  checked in the branch; a single-expression block is the expression itself and is.

  A public Java INSTANCE field read is guarded like any other platform value and names the field
  (`value must not be null`), matching kotlinc. Not yet guarded, because the value is not modeled as
  `T!` today (a front-end gap, not an emitter one): a Java STATIC field read (`System.out` types as
  `PrintStream`, not `PrintStream!`) and an index-operator result (`javaList[0]`). Also unimplemented:
  kotlinc's OTHER form, the message-less `Intrinsics.checkNotNull(Object)V`, which it emits wherever the
  narrowed value has no name to report — a plain read of a platform-typed local, a source-block branch,
  a `try` value, the merged value of a conditional in argument position; and the guard on a non-null
  Kotlin extension RECEIVER (`getenv(..).trim()`). krusty emits the NAMED form only, so a narrowing it
  cannot name stays unguarded rather than guarded under an invented name.
  Tests: `tests/platform_call_assertions_e2e.rs` (per-position `checkNotNullExpressionValue` call-site
  and message differential vs kotlinc, every guarded position run for its exception and message, and
  the top-level `<clinit>` repro).

  Measured over the 2.4.10 box corpus (per-file class-byte hashes with and without the guard, 3355
  files krusty compiles): 46 files change, none flips compile status, and 38 of them place the same
  guards kotlinc does. (A 47th differs between any two runs of the SAME binary — a pre-existing
  non-deterministic emission, not a guard.) The 8 that differ all trace to two PRE-EXISTING typing
  gaps, not to the guard rule — in both directions the guard follows krusty's own type, so it is never
  wrong, only in a different place than kotlinc's:
  * A member kotlinc resolves on a Kotlin BUILTIN, which krusty resolves on the mapped Java class and
    therefore types `T!`: `toString()` reached through a `CharSequence`/`Throwable`/`Comparable`
    receiver (kotlinc: `kotlin.Any.toString(): String`), `Enum.name` (kotlinc: `kotlin.Enum.name:
    String`), and `MutableMap.put` (kotlinc's non-null builtin parameters). krusty guards a value
    kotlinc already knows is non-null (`kt42137.kt`, `kt65197.kt`, `kt15806.kt`,
    `nestedClassesInAnnotations.kt`, `eagerLambdaAnalysisWithNoExpectedType.kt`,
    `funWithTypeParameterWithUpperBound.kt`), or skips a narrowing kotlinc's builtin parameter creates
    (`forInArrayListIndices.kt`).
  * An INFERRED declaration type: kotlinc commits an expression-body function's inferred return to the
    NON-NULL bound of a flexible body, so the guard lands inside that function; krusty keeps `T!`
    there and guards at the caller's declared type instead (`collectionAssignGetMultiIndex.kt`). This
    is the same modeling gap as an inferred `val a = System.getenv("P")`, which krusty annotates
    `@Nullable` where kotlinc annotates nothing.
- `try { … } catch (e: E) { … }` (no `finally`): the body value (and each catch value) is stored into a
  result temp and loaded at the merge, like kotlinc. The protected region covers the body + result
  store; each catch is an exception-table handler whose StackMapTable frame has the caught exception on
  the stack and the pre-`try` locals. A diverging body/catch (`throw`/`return`) emits no dead store, and
  a fully-diverging `try` has no merge. try in a property initializer is skipped (constructor frame
  context). `throw e` → `athrow` (`tests/try_catch_e2e.rs`). In VALUE position the branch types merge
  with the full `join` — `try { x } catch { null }` is `T?`, two different reference classes merge to
  `Any` (or `Any?` when either branch is nullable), and the same class with differing type arguments
  erases to that class (`List<*>`) — but only
  for REFERENCE branches (the emitter's untyped merge slot models no primitive widening/boxing, so a
  primitive mismatch keeps the lenient statement merge, `Unit`)
  (`tests/try_catch_expr_nullable_merge_e2e.rs`, `tests/try_catch_expr_generic_merge_e2e.rs`). A `finally` block is inlined (like kotlinc)
  at each exit: the normal fall-through, the end of each catch, and a synthetic catch-all (any
  throwable) covering the body + catch handlers that runs the `finally` then re-throws. A `try` whose
  body/catch performs a `return`/`break`/`continue` out of the `try` (which must run `finally` first) is
  skipped. **Nested `try`/`catch` is supported** (a `try` in another `try`'s body or catch — verified
  end-to-end), **except when a `finally` is involved in the nesting**: a `finally` is inlined at every
  exit of its protected region, so when it sits inside (or wraps) another `try` the duplicated code lands
  in overlapping exception ranges and trips a verify error — so a nesting that involves any `finally` is
  rejected (skip), never miscompiled (`NestedTry` in `tests/feature_box_e2e.rs`).
- `as T` to a non-null reference type throws on `null`: `Intrinsics.checkNotNull(value, "null cannot be
  cast to non-null type <kotlin-name>")` then `checkcast` — matching kotlinc. The same null-check
  applies to a DEFINITELY-NON-NULL type-parameter target `as (T & Any)` — even on an unbounded
  (nullable-bound) `T`, the `& Any` intersection throws NPE on `null`
  (`tests/definitely_non_null_type_e2e.rs`). `as T?` and primitive
  casts are a plain `checkcast`/coercion. The safe cast `x as? T` lowers to
  `{ val t = x; if (t is T) t as T else null }` — `instanceof` then `checkcast` on a match, `null` on a
  mismatch (it never throws); the result is `T?`. The target must be a reference type (a primitive
  `as? Int` would yield the boxed `Int?` wrapper — not yet modeled, so it skips). `SafeCast` in
  `tests/feature_box_e2e.rs`. `is`/`as`/`as?` targets resolve through the **same** name→internal map the
  checker uses (`syms.class_names`), so a **classpath** type (`CharSequence`, `Number`, `Runnable`, a Java
  class) works, not just builtins and user classes. A class implementing a **generic classpath interface**
  (`Comparable<Foo>`) also gets the `ACC_BRIDGE` method the JVM needs (`compareTo(Object)` delegating to
  the specialized `compareTo(Foo)`): the interface's erased single-abstract-method comes from the library
  set's `sam_method`, and a bridge is added whenever the override's descriptor differs — without it an
  interface-typed call (`(x as Comparable).compareTo(y)`) faults with `AbstractMethodError`
  (`ClasspathIsAs` in `tests/feature_box_e2e.rs`). A literal-boolean `if` condition (`if (false) { … }`) is
  constant-folded (only the taken branch is emitted), like kotlinc's dead-code elimination.
- Generic functions (`fun <T> f(x: T): T`) erase the type parameter to `Object` in the JVM signature.
  At a call site, a result of erased type `Object` flowing into a more specific reference context (a
  typed `val`, a `return`, a function argument) gets a `checkcast` to that type — matching kotlinc (the
  value really is that type at runtime). `kotlin.Any`/`Object` targets get no cast.
- `vararg` parameters: the parameter's JVM type is the array (`Int...` → `[I`); a call packs the trailing
  arguments into a fresh array (`newarray`/`anewarray` + per-element store) and passes it, like kotlinc.
  Spread (`*arr`) is not modeled. `for (x in arr)` over an array iterates by index
  (`i = 0; while (i < arr.size) { x = arr[i]; …; i++ }`, array and size hoisted).
- **`vararg` and `Array` in `@Metadata`.** A `vararg`'s recorded `ValueParameter.type` is
  `Array<out E>` — the OUT projection is part of the record, and the unprojected element travels
  separately as `vararg_element_type`. Independently, `kotlin/Array` in ANY signature position
  (parameter, extension receiver, return) forces an explicit `JvmMethodSignature.desc`, because a
  reader derives descriptors by mapping class NAMES through a flat table and an array's descriptor
  depends on its type argument. The specialized primitive arrays (`IntArray` → `[I`) are in that
  table and record nothing, so `vararg xs: Int` records no descriptor while `vararg xs: Payload`
  does. The rule keys off the array, not off `vararg`. Tests:
  `tests/metadata_array_signature_e2e.rs` (byte-identity vs kotlinc 2.4.10); table in
  `docs/METADATA_NOTES.md`.
- **A class records only the supertypes source DECLARED.** An undeclared `kotlin/Any` is never a
  `Class.supertype`, generic or not — even though a generic class's JVM `Signature` attribute must
  materialize that superclass position, so the recorded generic signature krusty reuses for the
  supertype list always leads with it. The metadata emitter drops it. Tests:
  `a_generic_class_with_no_superclass_lists_only_its_interfaces` in
  `tests/typealias_abbreviated_type_e2e.rs`.
- **Classpath Java varargs (`T...`)**: the class reader carries `ACC_VARARGS` into `CallSig::vararg`.
  The shared call-argument lowerer then packs trailing elements into the final array parameter for
  both static and instance calls. An ordinary array parameter remains fixed-arity.
- Range expressions as **values**: `a..b` and `a..<b` are the only true range *operators* (parsed at a
  precedence tighter than infix functions, looser than additive). `a..b` over `Int`/`Long`/`Char`
  constructs the matching stdlib range object via `new IntRange/LongRange/CharRange(II/JJ/CC)` (kotlinc's
  intrinsic constructor); `a..<b` lowers to `RangesKt.until(…)`, returning the same range type. The
  result type is `kotlin.ranges.IntRange`/`LongRange`/`CharRange`; members like `.first`/`.last` resolve
  to the classpath `getFirst`/`getLast` getters. `until`/`downTo`/`step` are **not** operators — they are
  ordinary stdlib infix functions and parse as infix calls (`a until b` → `a.until(b)`), resolved through
  the library set like any extension call. A `for (x in r)` over a stored `IntRange`/`LongRange` value
  iterates as a counted loop (`last = r.getLast(); i = r.getFirst(); while (i <= last) { x = i; …; i++ }`),
  matching kotlinc's specialized loop and avoiding per-element boxing; `Char` ranges and progressions use
  the iterator protocol. The syntactic `for (i in a..b)` counted loop now spans `Int`/`Long`/`UInt`/
  `ULong`/`Char` counters (not just `Int`): the counter takes the uniform bound type, signed/`Long`/`Char`
  compare with the direct opcode, and the unsigned case compares with `Integer.compareUnsigned`/
  `Long.compareUnsigned` (a signed `<=` would misorder values past the sign bit). `tests/range_value_e2e.rs`.
  The `for`-range header parses the iterable at additive precedence so a trailing `..`/`until`/`downTo`
  is handled by the range path; when the iterable is **not** a `..` literal (a stored progression, a
  `(a..b).reversed()`, a chained `… step n step m`), the header continues the trailing `step`/infix
  calls itself (`progression.step(n)`) and iterates the result as a plain `for-each`, rather than
  stopping at the bare iterable and reporting `expected ')'`.
- **Reference array literals** `arrayOf(a, b, c)`: lower to the same `Vararg` IR node `intArrayOf` uses,
  which the backend allocates as `T[]` and fills element-by-element (the element type is the array's
  erased element; a logical primitive element is boxed at the store boundary, so `arrayOf(1, 2)` is
  `Array<Int>` backed by `Integer[]`, distinct from primitive `IntArray`). An empty `arrayOf()` obtains its
  element type from an enclosing declared `Array<T>` context, including property and local initializers,
  function/getter returns, assignments, and arguments; the backend therefore emits the specialized JVM
  `T[]` instead of an erased `Object[]`. The array creators
  (`arrayOf`/`intArrayOf`/…/`IntArray(n)`/`emptyArray`) are **compiler intrinsics** — they have no
  callable body in `kotlin-stdlib` (kotlinc's backend lowers them to array bytecode by resolved symbol),
  so krusty recognizes them the same way kotlinc does: by the **resolved stdlib symbol**, gated on the
  name *not* being shadowed by a user-declared function or local (a user `fun arrayOf` wins, never the
  intrinsic) — not by bare source name. An element that lowers to a
  branch — an `if`/`when`/elvis, a **safe call** `c?.calc()`, a relational comparison — is rejected on the
  vararg-pack lowering paths (the file skips): `is_branchy` treats those as non-spliceable (`ArrayOfRef`
  in `tests/feature_box_e2e.rs`). This is a *conservative lowering* restriction, not a verifier one — the
  emitter now types the held `[array, array, index]` into a mid-fill element's frames (see "An operand
  held on the stack across a branchy sub-expression must be TYPED into its frames"), so the shapes that
  do reach emit are verifiable. `try` must stay rejected regardless: a handler CLEARS the operand stack,
  so the partly-built array held there would be lost. `is_branchy`'s `==`/`!=` arm fires only when the
  LHS is *syntactically* a primitive literal (`file_expr_is_jvm_scalar`), so `listOf(x == y, …)` over
  `Int` parameters is NOT declined and does reach emit — that gap is what exposed the frame bug.
- **Enum reflection intrinsics** `enumValueOf<E>(name)` / `enumValues<E>()`: the checker requires an
  enum type argument and types the result as `E` / `Array<E>`. The synthetic registry emits
  `E.valueOf(name)` / `E.values()`, including through an expanded reified inline function. A reified
  inline function returning `T` (e.g. a `safeEnumValueOf` wrapper) is checked inside the body against
  T's erased bound (`Enum`): the expansion's result slot is typed by that erased return and the
  expansion's value is cast back to the call-site type (the `checkcast` kotlinc emits after a reified
  call), keeping branch-merge frames consistent (`tests/enum_value_of_intrinsic_e2e.rs`).
- **Primitive-array init constructor** `IntArray(n) { i -> elem }` (and `Long`/`Double`/`Float`/`Boolean`/
  `Char`/`Byte`/`Short`): kotlinc inlines the index lambda into a fill loop, which krusty reproduces by
  desugaring to `{ val n = <size>; val a = new T[n]; var i = 0; while (i < n) { a[i] = <body[it:=i]>; i++ }; a }`
  — reusing the existing size-alloc and `kotlin/Array.set` intrinsics (the backend selects `iastore`/… by
  the array's element type). The single lambda parameter is the **index** (bound to the loop counter); the
  body yields the element. The element value is spilled to a temp before the store, since a branchy body
  (`{ it % 2 == 0 }`) records a stackmap frame and `Array.set` pushes the array+index before the value —
  without the spill those would be stranded across the frame (VerifyError). Reference `Array<T>(n) { … }`
  allocates via the `NewArray` IR node (`anewarray`); a *primitive* `Array<Int>` (boxed `Integer[]`) is
  skipped. `PrimArrayInit`/`RefArrayInit` in `tests/feature_box_e2e.rs`.
- **`x == null` / `x != null` compile to `ifnull` / `ifnonnull`** (kotlinc's bytecode), regardless of the
  operand's static value type. A reference `==`/`!=` against the `null` literal must NOT go through the
  primitive `if_icmp*` path — `if_icmpeq` on a reference operand is only accepted by the verifier when no
  stackmap frame pins the operand types (it "works" until a nearby branch forces a frame, then
  `VerifyError: Bad type on operand stack`). `Intrinsics.areEqual` is reserved for two reference operands
  neither of which is the `null` literal. `records_frame` accounts for the `ifnull` branch+merge frame.
- **A comparison that PRODUCES a `Boolean` fuses its test exactly like one that drives a branch.** Both
  positions share one rule: never materialize an `iconst_0` just to feed a two-operand `if_icmp*` when a
  single-operand branch already says the same thing.
  - `Long`/`Double`/`Float` compare 3-way through `lcmp`/`dcmp*`/`fcmp*`, whose result is *already* -1/0/1
    relative to zero, so the test is the single-operand `ifeq`/`ifne`/`iflt`/`ifle`/`ifgt`/`ifge` family.
    `a == b` on `Long` (and therefore on `ULong`, which compares its carriers) is `lcmp; ifne`, **not**
    `lcmp; iconst_0; if_icmpeq`. Same for the `Double` and `Float` pairs (`dcmpg; ifne`, `fcmpg; ifne`),
    with the NaN-correct variant still chosen per operator (`a > b` is `dcmpl; ifle`). For `!=` on
    `Double`/`Float` krusty is *shorter* than kotlinc, which materializes `==` and then negates it with a
    second branch pair — an accepted divergence in the same family as the `ixor` one below.
  - The int category fuses the same way against the literal `0`: `a != 0` is `iload_0; ifeq`, never
    `iload_0; iconst_0; if_icmpne`.
  - **Zero on the LEFT fuses only for `==`/`!=`.** `0 == x` is `iload x; ifne` (kotlinc's shape), but
    kotlinc does NOT mirror the ORDERING operators, so `0 < x` stays the two-operand
    `iconst_0; iload x; if_icmpge`. Both value and branch consumers now go through the same
    non-structural comparison classifier and numeric operand emitter; the former branch-only
    `swap_cmp` exception was removed so identical comparison IR cannot acquire a different opcode shape
    from its surrounding position.
  - This previously held only for comparisons in *branch* position (`if`/`while`/`when` conditions, via
    `emit_compare_branch`); the value-producing path (`emit_compare`) always pushed the zero. Surfaced by
    diffing unsigned `equals` against kotlinc.
- **Value-position comparisons branch on the NEGATED condition to a `false` arm** — kotlinc's polarity:
  `if_icmpne L; iconst_1; goto E; L: iconst_0; E:`, i.e. fall through to *true*. krusty previously jumped
  to the *true* arm (`if_icmpeq L; iconst_0; goto E; L: iconst_1`). Semantically identical and the same
  instruction count, but matching costs nothing (one flip in the shared tail, `materialize_cmp_bool`) and
  makes the null (`ifnonnull`), referential (`if_acmpne`) and numeric (`if_icmpne`/`ifne`) arms match
  kotlinc, so the differential harness stops reporting permanent noise there. The null check runs BEFORE
  the referential arm, so `a === null` is `ifnonnull` and not `aconst_null; if_acmpne` — the same ordering
  `emit_compare_branch` already used, and the reason `lhs_null`/`rhs_null` are computed up front.
  - Known exception, **pre-existing and not fixed here**: `===` between a reference and a *primitive*
    (`a: Any === b: Int`, which kotlinc only warns about) reaches the numeric tail unboxed, because
    `int_cat` treats every non-`Long`/`Double`/`Float` type as int-category. That emits an int branch on
    a reference and fails verification. Same in both positions, and on the pre-change compiler.
  - Fixing the merge-point accounting (`set_stack` at the false arm, previously applied only to the
    numeric arm) also removed a permanent `+1` drift in the null/referential arms. That drift made a
    LATER branchy inline splice in the same expression see a non-empty baseline and refuse, escalating to
    a hard `inline splice failed` compile error — so e.g.
    `two(a === b, x.takeIf { it > 0 }.toString())` now compiles.
  `tests/bytecode_parity_e2e.rs`: `long_compare_in_value_position_tests_lcmp_without_materialized_zero`,
  `unsigned_long_equality_tests_lcmp_without_materialized_zero`,
  `double_compare_in_value_position_tests_dcmp_without_materialized_zero`,
  `float_compare_in_value_position_tests_fcmp_without_materialized_zero`,
  `compare_against_zero_in_value_position_is_single_operand_branch`,
  `zero_on_the_left_in_value_position_fuses_only_for_equality`,
  `zero_on_the_left_in_branch_position_fuses_only_for_equality`,
  `referential_null_comparison_in_value_position_is_single_operand`,
  `value_position_comparison_does_not_poison_a_later_inline_splice`,
  `value_position_comparison_polarity_matches_kotlinc` (branch position:
  `compare_against_zero_is_single_operand_branch`).
- **Accepted divergence — reference `!=` in value position uses `ixor`.** For `a != b` on two non-null
  references krusty emits `Intrinsics.areEqual; iconst_1; ixor`; kotlinc emits the four-instruction branch
  form `areEqual; ifne L; iconst_1; goto E; L: iconst_0; E:`. krusty's is two instructions shorter and
  provably equivalent (`areEqual` returns a `Z`, i.e. 0 or 1, so `xor 1` is exactly logical negation), and
  unlike the cases above it is not a redundancy to remove — so it stays. Recorded here so a future ABI
  diff against kotlinc reads it as intentional rather than a bug.
- **A class method's expression-body return type is inferred with its own parameters in scope**
  (`fun m(x: Int) = x + 1` → `Int`). Signature collection adds the method's parameters (alongside the
  class properties) to the literal-inference scope; previously only the properties were visible, so a
  body referencing a parameter inferred `Unit` and then tripped a return-type mismatch against the body.
  This also unblocks a **bound method reference** `obj::m` whose method has an inferred return.
- **Inferred returns are recorded per overload, keyed by `(name, parameter types)`** (not name alone), so
  two same-name overloads with different inferred returns don't clobber each other and a call binds the
  right overload's return (`tests/overloaded_inferred_return_e2e.rs`). The key uses the SELECTED
  signature's params at every site — `resolve_ty` + vararg→array at the insert (matching
  `collect_signatures`), `fi.callable.params` at the call-site read, `sig.params` at codegen — so a
  reference-bounded type parameter (`fun <T : Number> show(x: T) = x.toString()`) erases to its bound
  consistently across all three; a key rebuilt from the raw AST in codegen (`ty_of`, which erases a bare
  type parameter to `Object`) would diverge and make codegen miss the override
  (`tests/generic_inferred_return_e2e.rs`).
- **An inferred generic return bound to a primitive types as the plain primitive** (`fun <T> fizz(x: T): T;
  fizz(1)` is `Int` — usable at an `Int` parameter, in arithmetic, as an `Int` initializer), matching
  kotlinc's static type. The runtime value behind the erased `Object` return is still the boxed wrapper;
  the lowerer's erased-return coercion (`has_scalar_value_repr(st)` on an erased-top physical return)
  unboxes the call result once, so every use sees the real scalar and a reference context re-boxes it
  (`tests/generic_inferred_primitive_return_e2e.rs`). An EXPLICIT type argument (`underlying<Int>(a)`)
  types the same way (`explicit_generic_return`, previously boxed-nullable). Both paths keep a
  DECLARED-NULLABLE return (`fun <T> foo(...): T?`) boxed (`Int?` — the erased result may be null, an
  eager unbox would NPE). And a scalar-typed erased call result flowing straight back into a reference
  context of the SAME primitive (`val v: Int? = uncheckedCastNull<Int>()`) reuses the original boxed
  reference (`checkcast` only) instead of unbox+re-box — the round-trip is not the identity on `null`
  (kt84727: `null as T` must survive, kotlinc keeps the reference)
  (`generic_hof_vc_binding_e2e::nullable_generic_return_keeps_null`).
- **Return-only type parameters on `inline` functions use the expected type.** The inferred binding
  must satisfy its declared bound and is passed to the inline expander for reified operations. For a
  nullable return such as `T?`, inference removes the return nullability before binding `T`.
- **Conditional branches contribute result-type constraints to generic calls.** For `if`, `when`,
  and elvis expressions, a selected call with unbound result formals is rechecked against a sibling
  result type that can bind them. Branch order does not affect the binding. If no sibling can bind
  the formals, the cannot-infer diagnostic is reported at the call. Test:
  `tests/conditional_branch_inference_e2e.rs`.
- **A tail-call-forwarded suspend fn boxes its EARLY returns.** The tail-forward shape (no state machine,
  `$completion` threaded to the callee, callee's `Object` result `areturn`ed verbatim) also admits bodies
  with early exits (`if (n == 0) return true; return odd(n - 1)`); the CPS method returns `Object`, so the
  early primitive return boxes and a bare `return` in a `Unit` fn yields `Unit.INSTANCE`, exactly as in a
  leaf body — only the forwarded tail stays verbatim (kotlinc's shape). Previously the forward path
  skipped return boxing entirely (`iconst_1; areturn` → VerifyError)
  (`tail_forward_with_early_returns_boxes_them` in `tests/feature_coverage_s_e2e.rs`).
- **`return` inside a `try { … } finally { … }`** now runs each enclosing `finally` (innermost first)
  before transferring control, instead of bailing. The lowerer pushes the `finally` AST onto a
  `try_finally_stack` while lowering the body/catches, and a `Stmt::Return` inside inlines those finallys:
  `{ val tmp = <value>; <finally>…; return tmp }` — the return value is captured into a temp first so a
  `finally` that mutates state cannot change what is returned (Kotlin evaluates the value, then runs the
  finallys). `emit_try` still inlines the finally on the normal-completion and exception paths. A `break`/
  `continue` escaping the `try`, or a `finally` that declares locals (its duplicated slots would clash
  across the inlined copies), is still skipped. `ReturnInTryFinally` in `tests/feature_box_e2e.rs`.
  A `return` *inside* the `finally` itself (`try { return 0 } finally { return 1 }`, where the finally's
  return overrides the try's) inlines only the finallys that **enclose** it, never itself: each finally
  `i` is lowered with `try_finally_stack` truncated to `finallys[..i]`. Inlining a finally with itself
  still on the stack used to re-inline it at its own `return` and recurse until the stack overflowed.
  `finally_return_overrides_try_return` in `tests/finally_e2e.rs`; box corpus `try/finally6.kt`.
- **`when (subject)` with `in`/`!in` range branches** (`when (x) { in 4..6 -> … }`): the parser builds
  the structural `Is`/`InRange` node for an `is`/`in`-range condition (same as the infix `is`/`in`
  operator); the checker and lowering treat that node as a complete boolean test of the subject, not a
  value to compare with `==`. `in <range>` is the bounds-check intrinsic (`InRange` → `a <= x && x <= b`,
  no range allocation — matching kotlinc); `in <collection>` (a `contains` call) in a `when` is not
  modeled and skips — krusty recognizes the test forms *structurally*, never by matching a method name.
  `WhenInRange` in `tests/feature_box_e2e.rs`.
- **Mixed-primitive `a.compareTo(b)`** (`1.compareTo(1.1)`, `0.toByte().compareTo(5.0)`) → promote both
  operands to their common numeric type, then `{Integer,Long,Float,Double}.compare(a, b)` (returns -1/0/1);
  `Byte`/`Short`/`Char` compare in the `int` category. (A user `operator compareTo` has a reference
  receiver and is handled separately.)
- **A negated `Double`/`Float` literal is the negative constant** (`-0.0` → the `-0.0` `ldc`, `-2.5` →
  `-2.5`), not the `0.0 - x` desugar (which gives `+0.0` for `-0.0`, losing the sign that IEEE-754
  comparisons — `Double.compare(0.0, -0.0) == 1` — distinguish). `CompareToAndNegZero` in
  `tests/feature_box_e2e.rs`.
- **`kotlin.test` (and other default-argument) top-level calls.** A receiver-less library function call
  that omits trailing defaults (`assertEquals(a, b)` — the `message` is defaulted) resolves to the
  `name$default` synthetic (`resolve_callable` falls back to `find_top_level("name$default")` when no
  exact/vararg overload matches); the call lowers the provided prefix then appends a placeholder per
  omitted parameter, the `int` default-bit-mask, and the `null` marker — kotlinc's defaulted-call shape.
  A generic function whose provided parameters are mismatched primitives (`assertEquals(0, longVal)`)
  is skipped (kotlinc unifies the type variable and coerces the literal; krusty would box `Integer` vs
  `Long`). This is what compiles the large `kotlin.test`-based slice of the box corpus.
- **A nullable-primitive *field* smart-cast** (`if (value != null) value` where `value: Int?`) unboxes the
  wrapper on read, like the local-variable path — else the `Integer` reaches an `int` context (verify error).
- **A statement-position `when` may mix `Unit` arms with value arms** — kotlinc coerces every arm to
  `Unit`. The checked discarded-expression mark selects effect-then-`Unit` lowering; value-position
  mixes remain unsupported. Statement position does not rewrite the final arm to `else`, because a
  non-exhaustive `when` may match no arm.
  `tests/when_statement_value_arm_e2e.rs`.
- **A subjectless `when` threads false-branch narrowings into later arms**: a later condition or body
  runs only after every earlier arm fell through. Null, compound, and type-test conditions use the
  same narrowing rules as `if` branches.
  `tests/when_null_guard_smartcast_e2e.rs`.
- **`x ?: return` smart-casts `x` for the code that follows** (also `?: throw`/`break`/`continue`/a
  `Nothing`-typed call): completing an elvis whose right-hand side is `Nothing` proves a stable
  `val`/parameter non-null, exactly like an `if (x == null) return` guard. A nullable-primitive local
  narrows to its unboxed primitive (the lowerer's `Name` path unboxes the reference slot on use); a
  nullable reference already reads as its non-null type. A local `var` narrows like a `val` when no
  active capturing closure can mutate it (see the var smart-cast entry below); unsigned stays unnarrowed (its
  value-box unbox isn't modeled).
  `tests/elvis_return_smartcast_e2e.rs`.
- **`u?.member ?: return` smart-casts the safe-call ROOT receiver** for the code that follows: the
  elvis only completes when every `?.` in the left side held, which proves the chain's root non-null.
  The root must be a stable `val`/parameter name or a local `var` no active capturing closure can mutate; the
  same unsigned exclusions as the bare-name form apply. (Intermediate chain links narrow too when
  they are stable property paths — see the access-path entry below.) `tests/elvis_return_smartcast_e2e.rs`,
  `crates/krusty-lsp/src/compiler_analysis.rs::source_set_narrows_safe_call_root_after_elvis_return`.
- **Smart casts apply to stable ACCESS PATHS, not only plain names** (`tests/path_smartcast_e2e.rs`).
  `==`/`!=` null checks, `is`/`!is` type tests, and contract conclusions (`returns(false) implies
  (this != null)` — `if (a.p.isNullOrBlank()) … else { a.p.length }`; `require(a.p != null)`) narrow
  `this.p`, `a.p`, `a.b.c`, and `a?.p` reads in the guarded region, through one machinery: a
  condition is folded to a set of `(NarrowPath, Ty)` facts — a root binding plus property segments —
  applied at every site (`if`/`when`/`while` branches, `&&`/`||` right operands, early-return guards,
  contract statements, elvis guards) by the same `apply_narrowings`. A root-only fact shadows the
  binding (the classic mechanism); a segmented fact is recorded per scope frame and consulted when a
  member read is typed, the lowerer emitting its generic `checkcast`/unbox from the recorded type.
  kotlinc's stability rules gate every step: the root is `this` or a local `val`/parameter, or a
  local `var` no active capturing closure can mutate (see the var smart-cast entry below); each segment is a
  `val` (no setter) without a custom getter or delegate whose getter cannot
  be replaced at runtime — a final property is stable even on an open class, while an open property
  requires a statically final receiver type. Its type is substituted like the member read
  (`Box<T>(val v: T)` narrows through the receiver's actual type argument). A
  safe-call chain's proof covers every prefix (`a?.b?.c != null` narrows `a`, `a.b`, and `a.b.c`);
  a plain chain's covers the full path only; a safe-call chain ending in a METHOD (`u?.f() != null`)
  narrows just the root. Soundness invalidations: a fresh declaration of the root name drops the
  frame's narrowings rooted at it (a proof never transfers to a new binding); a `this`-rooted
  narrowing applies only while `this` is still the receiver it was proven against (never inside a
  receiver lambda or inner class); and the bare/`this.`-qualified forms of an own member `val`
  share one narrowing.
- **A local `var` smart-casts like a `val`** when no already-created capturing closure can mutate it
  (`tests/var_smartcast_e2e.rs`). Straight-line assignments replace the flow type, while writes in
  nested control flow join with the prior fact. Inline-spliced lambdas follow the same ordered flow;
  a lambda declared later does not invalidate an earlier proof. Assigning `null` narrows the read to
  `Nothing?`, while a null initializer keeps the declared type. Member selection still uses that
  declared type before reporting nullable-receiver diagnostics against the flow type. When an active
  capturing closure makes a cast unstable, every receiver use that needs it reports the exact
  smart-cast-impossible diagnostic instead of a generic unsafe-call error. The null branch may still
  narrow to `Nothing?` when every interfering closure write also stores null.
- **An `if`/`else if` chain of diverging guards narrows level by level** for the rest of the block:
  `if (x is A) return …; else if (x !is B) return …` proves `x !is A && x is B` afterwards, because
  falling through a level whose then-branch diverges means that level's condition was false. The walk
  stops at the first non-diverging then-branch (control can fall through it with its condition true).
  This is the statement form kotlinc handles via exhaustive flow typing; krusty walks the else-if
  spine only. `crates/krusty-lsp/src/compiler_analysis.rs::source_set_narrows_after_else_if_return_chain`.
- **`x is Int? && x != null` narrows to the non-null primitive** (either leaf order): the `is Int?` leaf
  narrows to the nullable-primitive wrapper and a `x != null` leaf anywhere in the same `&&` chain strips
  the `?`. The refinement is pushed last, so the innermost-last declare keeps it over the `Int?` binding.
  `is Int?` alone still reads as `Int?`, and unsigned stays unnarrowed.
  `tests/is_nullable_and_notnull_smartcast_e2e.rs`.
- **A `finally { return … }` / `finally { throw … }`** that itself transfers control suppresses the
  catch-all's exception re-raise (emitting the dead `athrow` left an unframed instruction → verify error).
- **`is`/`as`/`as?` to `IntArray`/`CharArray`/…** resolves to the primitive array type before the
  classpath-class fallback (the JDK ships an unrelated `sun.jvm.hotspot.utilities.IntArray`). `is UInt`/
  `is ULong` and smart-casting a reference to an unsigned value type are rejected (value-type boxing).
- **A branchy arithmetic operand spills.** When one operand of a primitive `+`/`-`/`*`/`/`/`%`/bitwise/
  shift is branchy (records a stackmap frame — `5 + if (c) 1 else 2`, `r += if (…) … else …`), the
  emitter routes both operands through `emit_operands`, which stores the already-pushed operand to a temp
  so it isn't stranded on the operand stack across the branch's merge frame (`VerifyError: Inconsistent
  stackmap frames`). Non-branchy operands emit in place, so the common-case bytecode is unchanged.
  `BranchyArithmetic` in `tests/feature_box_e2e.rs`.
- **An operand held on the stack across a branchy sub-expression must be TYPED into its frames.** Where
  spilling to a temp isn't available — the store instruction needs its operands underneath it — the
  emitter keeps the held entries on the operand stack and records them in every stack-map frame the
  sub-expression writes (`pending_stack`, applied through `emit_value_over`). This covers the positions
  that fill a container element-wise: a `Vararg`'s `dup; index; <element>; aastore` loop (`[array, array,
  index]` held), the `SpreadBuilder`/`PrimitiveSpreadBuilder` `dup; <element>; add` loop
  (`[builder, builder]`), and `kotlin/Array.get`/`.set` (`[array]` under the index, `[array, index]` under
  the value). A comparison in such a position (`listOf(x == y, x != y)`, `b[0] = x == y`) branches to a
  merge label whose frame previously declared an EMPTY stack; the class file still emitted successfully
  and only failed at link time (`VerifyError: Inconsistent stackmap frames at branch target N` /
  "Current frame's stack size doesn't match stackmap"). kotlinc also holds operands live across the
  element, just with full frames — and one fewer, since it `astore`s the array to a local and reloads
  it per element instead of `dup`ing it. All comparison arms are affected alike (numeric `if_icmp*`,
  referential `if_acmp*`, `ifnull`, and the `lcmp`/`dcmp*` three-way forms), since each records its own
  branch+merge frames. Where the position CAN spill instead — `Array.get`/`.set`, which start from an
  empty stack — an operand that must not be held at all (`must_spill_across`: a `try`, whose handler
  clears the operand stack) takes the `emit_operands` temp route; the `Vararg`/`SpreadBuilder` fill
  loops have no such option, and lowering declines a `try` element for them (`is_branchy`).
  `tests/comparison_under_operands_e2e.rs`.
- **A `lateinit` FIELD read is itself frame-recording.** The uninitialized guard kotlinc inserts at every
  such read (`dup; ifnonnull L; ldc name; invokestatic throwUninitializedPropertyAccessException; L:`)
  branches, and its join records a stack-map frame typing only the field value. So a `lateinit` read is a
  branchy sub-expression exactly like a comparison or a `when`, and every position that holds operands
  across one — `emit_operands`, `New`, `SetField`, `StringConcat`, and the `emit_value_over` fill/subscript
  positions above — must spill or type the held entries. `records_frame` answers this for `GetField` by
  the field's `lateinit` flag, and for `PropertyRead` by first resolving which realization the read takes:
  only a DIRECT FIELD load carries the guard inline, since a read through the accessor hides it inside the
  getter body (which is why a cross-class or inherited read, always an accessor read, was never affected).
  Recursing into the receiver alone answered `false`, so `class C { lateinit var s: String; fun f() =
  listOf(s, s) }` emitted successfully and failed at link time with `VerifyError: Inconsistent stackmap
  frames at branch target N`. This is emitter-only: the guard shape, and the fact that a `lateinit` read
  still throws while the field is null, are unchanged — spilling only moves *when* the earlier operands
  are evaluated relative to it. `lateinit` on a top-level/`object` property is a separate, still-declined
  shape (the IR backend skips the file). `tests/lateinit_operand_stack_e2e.rs`.
- **`===`/`!==` on a nullable-primitive operand is rejected** (skip): boxed identity vs the unboxed
  primitive — and `Double`/`Float`'s `-0.0`/`NaN` — has subtle semantics krusty doesn't model.
- **Dead-code elimination after a diverging statement.** Statements following a `return`/`break`/
  `continue` or an expression of type `Nothing` (a `throw`, or a call that never returns) in the same
  block are unreachable; krusty drops them (and a trailing block value), matching kotlinc. Emitting them
  would leave a dead branch target without the stackmap frame the JVM verifier requires (`VerifyError:
  Expecting a stack map frame` — seen with `try { throw …; <unreachable> } catch …`).
- **Dead-code suppression in the emitter — divergence in VALUE position.** The rule above is a lowering
  decision about *statements*; it cannot cover a diverging expression used as a VALUE, because the
  consuming construct always emits opcodes after the value: a local's `istore`, an outer call's
  `invokevirtual`, a method's implicit `return`. When the value diverges, those trailing opcodes are dead
  straight-line bytecode the verifier rejects. `CodeBuilder` therefore tracks reachability directly: after
  an unconditional terminator (`goto`, `athrow`, any `*return`) instructions are DROPPED until control can
  demonstrably arrive again. Operand-height tracking, `max_stack`, and `max_locals` keep running while
  dead, so a resumption point sees the state it would have seen anyway; `LineNumberTable`/
  `LocalVariableTable` entries that would land in (or one past) a dropped region are dropped with it,
  since their `start_pc` must index the code array. Because this is a property of the instruction stream,
  no consuming construct needs its own divergence check — `boom()?.hashCode()`,
  `val y: Int = boom() ?: 1`, `println(boom())`, `boom().toString()`, `if (true) { boom(); 1 }`, and a
  BRANCHY sibling (`g(boom(), if (b) 1 else 2)`, the `when`/`&&`/`try` spellings, an inline-spliced
  `5.let { … }`) are all the same case.
  **What counts as arrival is the whole design.** Binding a label revives ONLY when some
  already-emitted branch targets it (a recorded fixup). A branch emitted while dead was itself dropped
  and left no fixup, so its target stays dead and the rest of that construct is dropped with it — without
  that rule, `g(boom(), if (b) 1 else 2)` resurrects the `else` arm and the `istore`/`invoke` tail around
  the hole where its condition used to be (`VerifyError: Bad local variable type`). A backward target
  (a loop head) is bound before its back-edge and so never revives: reaching the head while dead means
  the whole loop is unreachable. An EXCEPTION HANDLER has no incoming branch at all, so it binds through
  `bind_handler`, which revives on whether its protected range holds live emitted bytes — that is exactly
  the `try` whose body diverges (dead at the handler, yet the handler runs), while a `try` that is itself
  inside a dropped region guards nothing and goes with it. A label bound inside a dropped region sits at
  the same offset as the next live instruction, so its frame is dropped too: registered first, it would
  otherwise out-rank the live label's frame in `build_stackmap`'s same-offset dedup. An inline splice in a
  dead region is dropped as well — its relocated frames are bound INSIDE the body, never at its first
  byte, so emitting it would leave an unreachable region with no entry frame; `bind_at` is a no-op while
  dead and every consumer (`resolved_frames`, `build_stackmap`, `resolved_exceptions`) drops entries for
  an unbound label.
  Relatedly, a `Nothing`-returning REAL call is emitted with zero result words
  (`slot_words(Nothing) == 0`) yet physically leaves a `Void`; the terminating
  `throw KotlinNothingValueException()` re-declares that word before discarding it, or `max_stack` is
  undercounted by whatever sits beneath it (`VerifyError: Operand stack overflow` on `println(boom())`).
  (`tests/diverging_value_position_e2e.rs`.)
- **A `for`-range `step` is evaluated exactly once** (hoisted to a temp before the loop), not per
  iteration — a side-effecting `step` (`a until b step sideEffect()`) must run a single time, matching
  kotlinc's evaluation order. `DeadCodeAndStep` in `tests/feature_box_e2e.rs`.
- **Inferred return type from a method call** (`fun b() = a()`, `this.a()`, or an inherited method): the
  expression-body return-type inference scope is seeded with this class's and its superclasses' methods
  that have an *explicit* return type, so a sibling/`this`/inherited call resolves. (A *chained* inference
  where the callee is itself an inferred-body method — `fun b()=a(); fun c()=b()` — isn't resolved; the
  callee needs an explicit return. Top-level function-call inference was already supported.)
- **Bare access to INHERITED members** from a subclass method (`fun f() = x` / `x = …` / `x++` where `x`
  is declared in a superclass): the checker resolves bare reads/writes/inc-dec through the class's
  superclass chain (`lookup_prop`/`prop_of` already recurse; the `Assign`/`IncDec` checkers now consult
  `this`'s class chain, not just locals + top-level props). At signature-collection time the superclass
  chain's backing-field properties are added to the expression-body return-type inference scope, so
  `fun f() = inheritedProp` infers its type. Inherited writes and `++`/`--` lower through the property
  getter/setter (an own field stays a direct `getfield`/`putfield`). `InheritedMembers` in
  `tests/feature_box_e2e.rs`. (An inferred return from an inherited *method call* — `fun f() = inheritedFn()`
  — is still not inferred; annotate the return.)
- **Bare `x++` / `x--` on a `var` field** (implicit `this.x`, statement position): `this.x = this.x ± 1`
  via a direct field read/write inside the owning class, reusing the local-`++` `Byte`/`Short`/`Char`
  width-wrap (widen to `Int`, op, narrow back). The field's type comes from `syms.prop_of`. (`obj.x++` and
  `arr[i]++` were already parser-desugared to a compound assignment; a non-`var` or external-`this`
  receiver isn't handled here.) `MemberIncDec` in `tests/feature_box_e2e.rs`.
- **`x++` / `--x` on a TOP-LEVEL `var`** (statement and expression position): the read routes through the
  computed `getX()` accessor / `getstatic` / another file's facade getter, the write through the computed
  `setX(v)` / `putstatic` / facade setter (an enclosing class's member of the same name still binds first,
  kotlinc scoping; the checker rejects a member target in expression position). Bytecode matches kotlinc's
  shapes exactly: a decrement ADDS a `−1` constant (`iconst_m1`/`ldc2_w −1` + `iadd`, never `isub`); a
  POSTFIX spills the original value to a temp local (the expression value when used); a PREFIX stores and
  re-reads (statement position pops the dead re-read — kotlinc emits it too); `Byte`/`Short`/`Char` narrow
  after the add (`i2s` etc.). `Stmt::IncDec` carries `prefix` solely for this shape parity. Remaining
  file-level divergences are pre-existing and global (method emission order, local-slot reuse, `<clinit>`
  zero-init elision). Built-in numeric scalars only — a user/extension `inc`/`dec` operator on a top-level
  `var` still skips. `tests/toplevel_prop_incdec_e2e.rs`.
- **`LineNumberTable` for regular function bodies** (kotlinc parity, byte-verified): one entry per
  STATEMENT at its first pc (a block's TRAILING expression counts as a statement); an expression
  body maps to the expression's line; a `Unit` fn's implicit `return` maps to the closing-`}` line
  (`FunDecl::body_close_line` → `IrFile::fn_close_lines`); the first entry of a guarded function
  starts where kotlinc's does relative to the `checkNotNullParameter` prologue. Plumbed as parser
  line vecs (`File::{expr,stmt}_lines`) → sparse `IrFile::expr_lines` noted on each statement's
  FIRST lowered root (`append_stmt`) and on trailing values (`note_expr_line`) → `CodeBuilder::
  mark_line` in both emitter Block arms (same-pc overwrite, same-line dedupe). `<init>`/`<clinit>`
  keep their CURATED tables (marks are dropped in `add_method_sig`; the class-decl-line/initializer
  entries own those methods); a mark-less synthesized body keeps the single decl-line fallback.
  OUT OF SCOPE (documented residuals): inline-function SMAP line mapping, `LocalVariableTable` for
  top-level fns (next slice), the loop-head extra StackMapTable `same` frame.
  `tests/lnt_parity_e2e.rs` (6 full-byte + 3 javap-level pins).
- **`LocalVariableTable` for regular function bodies**: block locals end at block exit; method
  locals, `this`, and parameters span to method end. Parsed non-suspend functions record source
  local names through `IrFile::value_names`; synthesized and suspend methods retain their existing
  tables. Metadata string tables merge consecutive plain records, and method attribute names use
  ASM's `StackMapTable`-before-debug-table order. Remaining byte-parity differences include dead
  slot reuse, branch fall-through elimination, and inline-local name mangling.
- **Receiver scope functions `run`/`apply`** (the receiver is `this`, not `it`): the lowerer inlines the
  body binding the receiver to a `this` slot with `cur_class` cleared, so the body's bare member reads
  (getter), writes (setter), and method calls (`invokevirtual`) all resolve against the receiver through
  *external* access — the inlined code runs in the caller, not inside the receiver's class, so its private
  backing fields aren't directly reachable. `run` yields the body value, `apply` the receiver. Restricted
  to a user-class receiver (a library receiver, whose members aren't reachable through a bare `this`,
  falls through to skip). `run`/`apply` are excluded from the bytecode-splice route (which mishandles the
  receiver lambda). `ApplyRun` in `tests/feature_box_e2e.rs`. (`let`/`also` — value lambdas, param `it` —
  are unchanged.)
- **`++`/`--` as an expression value** (`val a = i++`, `++i`, and in operand position — a call argument,
  a string template, a `when` subject): a single `Expr::IncDec { target, dec, prefix }` node, usable
  anywhere an expression is; statement position keeps the `Stmt::IncDec` / member-index-assignment desugar.
  The value lowering uses no temp slot — the update is `i = i ± 1` and the value is the new `i` (prefix) or
  new `i` ∓ 1 = the old `i` (postfix), valid for every numeric type. `tests/incdec_expr_e2e.rs`.
- **Unsigned types `UByte`/`UShort`/`UInt`/`ULong`** — Kotlin inline classes over `Byte`/`Short`/`Int`/`Long`;
  unboxed they ARE that JVM primitive (descriptor `B`/`S`/`I`/`J`), with unsignedness driving
  operation/conversion choice (kotlinc hardcodes these intrinsic mappings, so krusty mirrors them). Literals
  `1u`/`0xFFuL`; `+`/`-`/`*`/`==` use the signed two's-complement opcodes; `/`/`%`/`<`/`>` use
  `Integer.{divide,remainder,compare}Unsigned` (`Long.*` for `ULong`); `toString`/templates use
  `Integer.toUnsignedString`; `UInt.toLong()` zero-extends via `Integer.toUnsignedLong` (not the
  sign-extending `i2l`); `toInt`/`toUInt` reinterpret (no-op). Boxing into a reference context uses the
  inline-class factory `kotlin/UInt."box-impl"(I)Lkotlin/UInt;` (and `unbox-impl` on read, `is UInt` →
  `instanceof kotlin/UInt`) — never `Integer`, so identity and large values are preserved.
  `tests/unsigned_e2e.rs`, `tests/feature_coverage_i_e2e.rs`.

  Still unmodeled, all of them REJECTED or skipped rather than miscompiled: `UIntRange` value iteration;
  and, for the narrow pair specifically, a `when` on a `UByte`/`UShort` subject (the arms-must-be-literals
  gate can't be satisfied — a bare `200u` arm types as `UInt`, and `200u.toUByte()` is not a literal),
  `is UByte`/`is UShort`, `UByteArray`/`UShortArray`, ranges and `in`-tests, `hashCode()`, the bitwise
  members (`and`/`or`/`inv`), a mixed-width operand pair (`UByte + UInt`), and an operator called by name
  (`a.plus(b)` — the checker doesn't surface the narrow receiver's metadata overloads). One known
  DIVERGENCE, not a skip: the native unsigned types do not carry kotlinc's value-class NAME MANGLING on a
  function that takes one — krusty emits `f(byte)` where kotlinc emits `f-7apg3OU(byte)`, pre-existing and
  shared by `UInt`/`ULong`.
- **Unsigned values at a CLASSPATH call boundary** — because an unsigned value has TWO representations
  (the carrier in a primitive slot, and the boxed inline class), every classpath call is a place where the
  representation the lowerer produced must agree with the descriptor the backend spells verbatim. Both
  directions are now pinned:
  - a value class krusty models as a scalar of its own is recovered from `@Metadata` as **that carrier**,
    not as the boxed class, so an argument to a value-class-MANGLED static keeps the erased form its
    descriptor declares: `maxOf(a, b)` on a `UInt` emits `iload; iload; invokestatic
    UComparisonsKt."maxOf-J1ME1BU":(II)I`, byte-for-byte kotlinc's shape, and compares in UNSIGNED order
    (the stdlib callee owns the comparator, so values past the sign bit order correctly);
  - `a.equals(b)` on an unsigned receiver never uses the `invokevirtual` form of the call. That
    instruction needs a REFERENCE receiver, so it forces a `box-impl` purely to have something to
    invoke on. The receiver stays the carrier in both directions:
    - between two values of the SAME unsigned type it is kotlinc's `equals` **intrinsic**: an unsigned
      value class wraps exactly one field, so its equality can only compare the carriers, and the call
      folds away to precisely the instructions `a == b` emits (byte-identical to krusty's own `==`, no
      box anywhere). Deliberately narrow to an argument of exactly the receiver's type — `Ty` equality
      including nullability, since `UInt?` is null-safe and a carrier compare is not;
    - every OTHER argument keeps the value class's own equality, reached through the static
      `kotlin/UInt."equals-impl":(ILjava/lang/Object;)Z` (`B`/`S`/`J` for the other three). It
      type-tests the argument first, which is what makes a cross-carrier comparison `false`
      (`UInt.equals(ULong)`, however the bits line up), a `null` argument `false`, and a `UInt?` one
      null-safe. The argument occupies the erased `Object` slot, so it arrives boxed however it was
      carried: an unsigned one through its own `box-impl` (never a Java wrapper — `equals-impl`
      type-tests it), a signed primitive through the wrapper, a reference unchanged.

      Two deliberate shape divergences live here, both against a kotlinc result that is a CONSTANT, and
      both answering that same constant without the box kotlinc pays for it:
      - the CROSS-CARRIER pair. kotlinc's primitive-`equals` intrinsic sees the two erased carriers,
        boxes both through the JAVA wrappers (`Integer.valueOf`/`Long.valueOf`) and calls
        `Intrinsics.areEqual` — `false` by construction, since a `java/lang/Integer` never equals a
        `java/lang/Long`. That is exactly what `equals-impl` answers for a `kotlin/ULong` argument, so
        krusty rides the one static rather than earning a second arm;
      - a LITERAL `null` argument — the ONE place kotlinc does box the receiver and emit `invokevirtual
        kotlin/UInt.equals` (its intrinsic declines the `Nothing?` argument). `equals-impl` answers the
        same `false` unboxed. Only the bare literal differs: a `null` held in an `Any?` goes through
        `equals-impl` in kotlinc too.

      Verified against kotlinc 2.4.10 with `javap` on all four unsigned types.

  Getting either wrong produced a class file that FAILED JVM VERIFICATION while krusty reported success —
  output strictly worse than declining the file, and invisible to a differential harness that checks
  compilation success. `jvm_can_emit` cannot see this class of defect: it inspects the TYPES a file
  mentions (and `kotlin/UInt` is fully supported there), not the representation of a value at a call
  boundary. The backstop therefore lives in the lowerer, where the descriptor and the lowered arguments
  are both in hand: `check_unsigned_boxes_fit_descriptor` declines the file
  (`gate:unsigned-box-in-erased-slot`) if a boxed unsigned would land in a primitive descriptor slot. It
  is a net, not the mechanism the supported shapes rely on — verified live by reverting the parameter
  recovery, which turns the miscompile back into a clean skip.
  `tests/unsigned_classpath_call_e2e.rs` asserts the backend contract directly (a decline passes; an
  EMITTED class that does not verify and run fails), so it keeps holding whichever way a shape is handled.
  Both the receiver box and that net rest on ONE question — *is this lowered value already a
  reference?* — which the checker's `Ty` cannot answer, since a value class and its carrier share one
  `Ty` on both sides of a box. Lowering answers it with a **representation query**,
  `lowered_reference_class`: the class a lowered node leaves on the stack, read off the node's own type
  (a callee's descriptor return, read from the provider's single `PlatformMethodLayout`; a
  cast's type operand; a field's declared type) and followed through the nodes that carry a value
  unchanged (a block's value, a `when` whose branches agree, a reference-to-reference coercion). A
  primitive-to-reference coercion does NOT claim its target class: the backend chooses a wrapper from
  the source carrier, and a broad target such as `Any` cannot prove which class was produced. It is not
  a match on the node that PRODUCED the value: a box that is cast or carried out of a block is still a box, and boxing it again
  would push a `Lkotlin/UInt;` at the `(I)` its own factory declares — the very `VerifyError` this
  section is about. The query is deliberately partial and one-sided: `None` means "a primitive carrier,
  OR a shape it cannot derive", so an unknown node keeps exactly the behaviour it had before that shape
  was understood, and a new shape can only ever remove a wrong box.

  A read of a LOCAL is the one carrier shape deliberately left unanswered. Its type lives on the
  declaring `IrExpr::Variable`, reachable only through a value-index table — and value indices are
  per-declaration-body and re-used (they restart at ~25 sites, are saved/restored around three nested
  bodies, and one coroutine temp is declared under the enclosing body's numbering). An entry surviving
  into the wrong scope would claim a box for a carrier and SKIP a required box, which is the same
  `VerifyError` from the other direction — a hardening measure that can itself miscompile is worse than
  none. Answering it soundly needs the value-numbering scopes made explicit first; until then the query
  returns `None` there, which is exactly the behaviour that shipped before it existed. No source shape
  is known that reaches a member call with an already-boxed unsigned receiver: every probed candidate
  (a nullable local via `!!`, a smart cast, a safe call, an erased map read, a `when` receiver, elvis)
  either declines or unboxes to the carrier first, so this remains a net rather than a live path.
  The net compares POSITIONS, so the lowered values have to be lined up with the descriptor slots first
  (`align_call_values_to_slots`). Two shapes carry a slot no lowered value fills, and both were measured
  over the box corpus and the full e2e suite rather than assumed:
  - a value class's members are realized as mangled `-impl` STATICS whose descriptor spells the receiver
    as the LEADING parameter (`kotlin/Result.getOrNull-impl:(Ljava/lang/Object;)…`) while the receiver
    travels beside the arguments — the corpus hits this over a hundred times. The receiver is checked
    with the arguments there, since a value-class owner is exactly where the lowerer boxes it;
  - a `suspend` `$default` synthetic spells the CPS `Continuation` BEFORE the `int mask` + `Object`
    marker (`withLock$default(Mutex, Object, Function0, Continuation, int, Object)`) and the backend
    appends it at emit time. The plain suspend descriptor has already had its TRAILING continuation
    stripped, so only the `$default` form needs this.
  A packed vararg needs no reconciliation — the array is emitted before the values reach the check — so
  the earlier claim that it shifts positions was wrong; no such call was observed. Any shape the
  alignment cannot line up now declines whenever a box is on the stack at all, rather than skipping: a
  count mismatch is "no position is known", never "nothing to check".
  The runtime provider returns reference/primitive parameter positions, the unambiguous
  runtime-supplied continuation position, and the concrete object return class together as one
  `PlatformMethodLayout`; JVM descriptor syntax remains outside common lowering, and the descriptor is
  parsed once rather than by independent parameter, continuation, and return queries that could
  disagree.

  `tests/bytecode_parity_e2e.rs` pins the two `equals` SHAPES: the folded carrier compare, and
  `equals-impl` with an unboxed receiver — the latter across all four carriers (`B`/`S`/`I`/`J`) and
  across `Any`, `String`, `UInt?`, cross-carrier, and the literal-`null` divergence. It also pins that
  both lowerings evaluate the RECEIVER before a SUSPENDING argument: neither reaches
  `emit_library_member_call`, so each spills the receiver to a temp itself, or the coroutine pass
  re-evaluates it in the resume block after the argument has already run.

  Aligning that second shape surfaced a separate miscompile, since FIXED: an unsigned VALUE PARAMETER
  MANGLES the JVM name (`libU` → `libU-OzbTU-A`, and the synthetic `libU-OzbTU-A$default` is named
  from the mangled form). A source-name suspend set missed that bytecode candidate: the callable came
  back non-suspend, nothing threaded the `Continuation` its descriptor still spells, and the emitted
  `invokestatic` was one argument short — a class that links and fails verification. Suspend-ness is
  now projected from the SAME metadata declaration selected by JVM name and descriptor shape for
  arity, defaults, return type, and contracts. This both recognizes mangled suspend declarations and
  prevents their flag from leaking to an ordinary same-source-name overload. Both suspend call forms
  emit and run: the `$default` synthetic (an argument omitted) and the plain mangled method (every
  argument supplied); the synthetic fixture also pins the ordinary overload independently.

  A net stays behind it (`gate:unthreaded-continuation-slot`): if a callable is not marked `suspend`
  and its descriptor still spells a `Continuation` the lowered values do not fill, the file is
  declined rather than emitted a slot short. The test is the UNFILLED slot (one descriptor parameter
  more than the call has values, and that parameter a `Continuation`) rather than `$default`-ness, so
  a non-suspend callee that declares a `Continuation` parameter of its own fills every slot and is
  untouched. It is an ASSERTION, not a feature: no source shape is known to reach it, and reaching it
  means a classpath read failed to recognize a `suspend` callee — so it is deliberately untestable
  without injecting that fault, and must not be deleted as dead code. What IS pinned is that it does
  not over-fire (`a_plain_continuation_parameter_is_not_an_unthreaded_continuation`).
- **An unsigned value crossing an ERASED GENERIC result boundary** — `fun <T> ident(t: T): T`
  erases to `(Object)Object`, so `ident(5u)` pushes a boxed `kotlin/UInt` and the use site has to
  unbox it. The unbox for an unsigned box is its own inline class's, `checkcast kotlin/UInt;
  invokevirtual kotlin/UInt."unbox-impl":()I` — NOT the boxed-primitive `checkcast
  java/lang/Integer; intValue`, which throws `ClassCastException` at run time because
  `kotlin/UInt` is not an `Integer`. All four carriers behave alike, each through its own class
  (`kotlin/UByte."unbox-impl":()B`, …); the checkcast/unbox pair matches kotlinc instruction for
  instruction, while the surrounding code still diverges where it already did
  (`Integer.toUnsignedString` on a masked carrier rather than `UByte."toString-impl"`).

  The rule applies to EVERY erased reference boundary, not only calls: wrapper ADAPTER selection
  consumes the semantic scalar type first, and only then may slot/descriptor selection map that type
  to its JVM carrier. `semantic_scalar_adapter` is the emitter-side statement of that ordering. Thus
  a generic property result (`Pair<UInt, …>.first`) unboxes through `kotlin/UInt`, and an inline
  `FunctionN` argument/result (`listOf(5u).map { it }`) crosses its `Object` invoke slots as a boxed
  `kotlin/UInt`; neither is allowed to rediscover the wrapper from the later `int` carrier. Callable
  references, property references, and ordinary lambda objects obey that same `FunctionN` contract.
  `InvokeFunction` therefore retains its semantic parameter list as well as its return type: the one
  generic consumer can select argument and result adapters without branching on which closure object
  produced the value. Plain-lambda implementation methods explicitly unbox boxed unsigned parameters
  into carrier locals and box unsigned result tails; declared SAM methods instead follow their own
  physical descriptors. Property writes use the same adapter in the opposite direction. This is
  deliberately independent of source file, module, classpath provider, owner, accessor spelling, or
  inline host identity.

  Lowering's erased-call-result coercion follows the same semantic rule. The value-read coercion
  (`coerce_to_static`) already retained unsigned identity, which is why a map/indexed read was correct
  while the call-result route was not; the latter now emits the unsigned unbox before recording the
  call's logical carrier type. Strict verifier/runtime tests cover calls, properties, inline lambdas,
  and ordinary function values so a future decline cannot silently remove the adapter coverage.

  A library extension RECEIVER is physically its first argument, so it crosses exactly the same
  representation boundary as a source-written argument. Lowering realizes both through the shared
  argument coercion before call or splice selection: a scalar entering a reference parameter is boxed
  with its semantic adapter, nullable/reference values are preserved, and value classes retain their
  identity instead of becoming a box of the underlying primitive carrier. This matters for an inline
  scope call such as `5u.let { … }`: the spliced lambda parameter expects `kotlin/UInt`, and an
  `Integer.valueOf` box would pass verification but fail the lambda's entry cast.

  The rule is attached to the representation boundary, not to a particular unsigned class, callable,
  discovery source, or emitter splice. Consequently ordinary and inlined library extensions consume
  the same IR argument, while values produced inside the host remain independent — for example,
  `map` still obtains its already-boxed element from `Iterator.next()`. Strict runtime regressions pin
  literal, local, and call-result receivers plus the separate host-produced element shape; declining
  either case is not accepted as a substitute for realizing the boundary.

  A BOUNDED type parameter erases to its BOUND rather than to `Object` (`<T : Comparable<T>>` →
  `Comparable`), and kotlinc unboxes there identically. The two classpath call sites (an imported bare
  name, a fully qualified call) each decide separately whether a substituted result needs coercing at
  all, and both excluded unsigned deliberately — because the coercion they would have reached emitted
  the wrong unbox. With the unbox corrected, excluding them only left the box on the stack where the
  carrier belonged: a `VerifyError`, again with krusty reporting success. Both gates now admit
  unsigned. No stdlib call reaches this erasure — every `<T : Comparable<T>>` helper has an unsigned
  specialization (`maxOf(UShort, UShort)` selects `maxOf-5PvTz6A:(SS)S`) — so the test builds a
  fixture jar. The three gates (the plain call, the packed-vararg call, and the imported bare name)
  are now ONE predicate, `substituted_ret_needs_coercion`: spelling the same rule three ways is how
  the unsigned exclusion came to differ between them in the first place.
  `tests/unsigned_generic_erasure_e2e.rs` asserts a STRICTER contract than
  `tests/unsigned_classpath_call_e2e.rs` — every shape there must EMIT and run, not merely avoid a
  bad emit, because a decline would leave the unbox it exists to pin untested.
- **Mutable capture rejection** — a lambda that writes an enclosing function local is rejected (the file
  skips), because krusty lowers a non-inlined lambda to a closure class that cannot mutate the outer frame.
  This applies on **both** the direct-lambda path and the extension-call path (`listOf(…).forEach { s += it }`
  — previously the latter bypassed the check and silently miscompiled). A primitive lambda parameter is
  unboxed from the erased generic `FunctionN` signature (`mapIndexed`'s index is `Int`, not boxed `Integer`).
- `companion object` (methods only): a synthesized `C$Companion` class holds the companion methods as
  instance methods; the outer class `C` gets a `public static final Companion` field of that type, built
  in `C`'s `<clinit>`; `C.foo()` compiles to `getstatic C.Companion; invokevirtual`. The companion
  constructor is package-private so the outer `<clinit>` can call it (kotlinc uses a private constructor
  plus a `DefaultConstructorMarker` synthetic — a byte-parity gap, not a behavioural one). Companion
  properties are not yet modeled.
- A NAMED `companion object Default { … }`: the parser now keeps the declared name
  (`ClassDecl::companion_name` → `ClassSig::companion_name`), and both the checker and the lowerer
  resolve `Fmt.Default` exactly like `Fmt.Companion` (same singleton; kotlinc additionally REJECTS
  the `Companion` spelling when a name is declared — krusty is permissive there for now). The
  synthesized class/field keep the `$Companion`/`Companion` spelling; kotlinc names them
  `Fmt$Default`/`Default` — a tracked byte-parity gap. A companion whose base-class clause carries
  EXPLICIT full-arity arguments (`companion object Default : Fmt(Cfg(false), "default")`) is now
  modeled: the checker types the args (static context, outer `this` masked) so their calls are
  resolved, and the lowerer lowers each against the declared base parameter type into the
  synthesized `super(…)`; partial-arity explicit args (rest defaulted) still bail
  (`tests/classpath_ctor_vs_same_named_function_e2e.rs` exercises the whole shape krusty-built).
- Non-null reference primary-constructor parameters are guarded with `Intrinsics.checkNotNullParameter`
  at the start of `<init>` (before `super()`), matching kotlinc.
- Constructing a classpath (non-IR) class (`RuntimeException("x")`, an imported Java type): `new` +
  `dup` + arguments + `invokespecial <init>`, with the constructor descriptor resolved from the
  classpath. JDK `Throwable` types fall back to the `()`/`(String)` constructors (the classpath reader
  doesn't read jimage constructor descriptors yet, so classes whose `<init>` lives only in the jimage —
  e.g. `StringBuilder` — are skipped). `throw e` emits `athrow` (`tests/throw_e2e.rs`).

- **`inline fun` (same-module, user-defined):** expanded at each call site by the IR lowerer
  (`Lower::lower_inline_fn_call`), matching kotlinc's effect — value parameters bind to once-evaluated
  argument temps, and a lambda argument is inlined at the call sites of its function-typed parameter
  (`Lower::lower_inline_lambda_invoke`), so a lambda capturing a mutable local works with **no closure
  class emitted**. This is how K2 inlines a *same-module* body (it has the body as IR). Supported subset:
  no extension receiver, no reified/type parameters, no default/vararg parameters, and no non-local
  `return` (an inlined `return` would return from the caller — bailed). Anything outside the subset
  bails (the file is skipped, never miscompiled). Known gaps vs kotlinc: (1) the inline function is
  **not also emitted as a standalone method**, so the facade ABI differs (kotlinc emits the body for
  binary compat / reflective callers) — an ABI-parity gap, not behavioural; (2) **cross-module stdlib**
  `inline fun`s (`forEach`/`let`/`also`/`repeat`) exist only as jar *bytecode*, so they cannot be IR-
  inlined — they go through the JVM **bytecode splicer** (`src/jvm/inline.rs`), the kotlinc-JVM path
  (`MethodInliner`): read the callee's compiled body from the classpath jar and splice it into the
  caller, relocating the constant pool. The IR `Callee::Static` carries `inline` (from the resolved
  signature); `Emitter::try_inline_static` splices, falling back to `invokestatic` on any unsupported
  shape (never a miscompile). **Landed so far:** a *branchless, single-exit* body with no function-typed
  (lambda) parameter — `inline::splice_branchless` drops the trailing return (leaving the result on the
  stack to fall through) rather than rewriting it to a `goto`, so the spliced region needs no
  StackMapTable frame. Proven end-to-end against a real kotlinc-compiled library inline fn
  (`tests/inline_splice_e2e.rs`: the call is spliced, no `invokestatic` to the callee survives). **Branchy
  bodies** also splice: the callee's `StackMapTable` is decoded (`inline::decode_stackmap`) and relocated
  into the caller (`inline::splice_branchy`) — frame offsets remapped past the `shift_locals` resize and
  the prologue, the body locals prefixed with the caller's locals (`Emitter::verif_locals_upto`), pool
  refs re-interned, the join frame added where the redirected returns land. Restricted (v1) to primitive
  parameters and an empty operand-stack baseline (statement / `val x = f(...)`); else falls back. Proven
  against a real kotlinc `if/else` inline fn (`inline_splice_e2e`). Pending: lambda-argument splicing
  (splice the caller's lambda at the callee's `FunctionN.invoke` sites — retires the
  `forEach`/`let`/`also` desugars) → non-local return → invokedynamic relocation. Tested by the
  `UserInline` snippet in `tests/feature_box_e2e.rs`. Two soundness declines gate every splice: a
  `$default` body is never spliced (the caller's placeholder nulls would type its parameter locals
  `Object`, a VerifyError — the real call is verifier-correct), and a body referencing an
  `ACC_PRIVATE` method/field is never spliced (the member is legal only inside the defining class;
  kotlinc rewrites to a synthetic `access$…` bridge krusty does not model — the fallback real call
  stays in the class).
  **Cross-file source calls to `inline fun`s link as facade statics.** A same-file call
  splices the body; a call from ANOTHER file of the same module has no AST to splice, so the
  defining file lowers + emits the inline fun as a facade static (kotlinc's `public static
  synthetic` shape — an extension rides the static's arg0) and the caller emits a plain
  `invokestatic` via the existing `Callee::CrossFile` path. Emittability is gated twice —
  syntactically (non-reified, non-suspend) and semantically
  (`SymbolTable::inline_fn_facade_emittable`: the selected physical signature must be callable —
  including value-class receiver representation — and there must be no splice-only body shape:
  a lambda that is stored or returned rather than passed to a call, anonymous objects,
  `try`/`break`/`continue`, a labeled or expression-position `return`, `is`/`as` on a type
  parameter; a `contract { … }` block is erased, not a closure) — with the shared registration
  semantic predicate `SymbolTable::source_fn_has_callable_body` consumed by common IR lowering and
  `jvm::prepare_module_symbols`; the latter is shared by backend, survey, and conformance drivers.
  Unsafe call sites BAIL rather than miscompile: an unregistered (unemittable) callee, a lambda
  argument with a non-local `return` or a mutating capture, a callable-reference/anonymous-function
  argument, or an enclosing inline lambda parameter passed as a value (an ordinary function-typed
  variable is fine — its value is a real closure).
  (`tests/cross_file_inline_call_e2e.rs`).
- **Collection `+=` (read-only vs mutable).** `coll += x` mutates in place when a `plusAssign` operator is
  applicable to the receiver, else reassigns (`coll = coll.plus(x)`) — exactly kotlinc's augmented-assignment
  resolution, with NO mutability predicate. The read-only/mutable distinction (`List` vs `MutableList`) is a
  Kotlin-type fact that exists in no JVM descriptor (both erase to `java/util/List`); krusty keeps the Kotlin
  type in the front end (`kotlin/collections/{List,MutableList}`, decoded from `@Metadata` return types) and
  erases it ONLY at emit (`to_jvm_internal`). The Kotlin collection hierarchy (`MutableList : List,
  MutableCollection`) is read from `kotlin/collections/collections.kotlin_builtins` (a `PackageFragment`
  proto, resolved via its `QualifiedNameTable` exactly as kotlinc's `NameResolverImpl`), never hardcoded.
  Applicability is generic: a candidate whose Kotlin extension receiver (from `@Metadata`
  `Function.receiver_type`) is a collection type the receiver does not subtype is rejected — so
  `MutableCollection.plusAssign` applies to `MutableList`/`ArrayList` but not to a read-only `List`. For a
  mutable receiver the inline `plusAssign` body is spliced (`add`/`addAll`). Tested:
  `feature_box_e2e::CollectionPlusAssign` and `tests/metadata_return_types.rs` (hierarchy parse, subtyping,
  `plusAssign` receiver).

- **Language-feature flags (`-XXLanguage:` / `// LANGUAGE:`) + name-based `[a, b]` destructuring.** A
  drop-in honors kotlinc's feature toggles: `krusty::features::LangFeatures` holds the enabled
  `LanguageFeature` names, sourced from `-XXLanguage:+Foo`/`-Xname-based-destructuring` CLI flags and (in
  the test harness/gate/survey) from `// LANGUAGE:` directives. Default = no experimental features, so
  default-flags behavior matches kotlinc. LSP project sync also reads recognized task-level Gradle
  arguments, unions module feature sets for project-wide analysis, applies explicit server flags in
  order, and applies source directives last in the compiler worker. First gated feature,
  `NameBasedDestructuring`: `for ([a, b] in e)` and `val/var [a, b] = e` are accepted ONLY when
  enabled, parsing identically to the `(a, b)` forms
  — both desugar to positional `component1()/component2()` calls, byte-identical to kotlinc (verified vs
  `-Xname-based-destructuring=complete`). Without the flag, `[a, b]` is rejected (kotlinc errors that the
  feature is experimental). A `var` destructured component captured and written by a closure is boxed
  into a `Ref` exactly like a plain captured `var` local (`var [a,b]=A(); val f={a=3}; f()` sees `a==3`).
  Tests: `multiDecl/*` box corpus (+96 gate), `tests/name_based_destructuring_e2e.rs`.

- **JPS (`.idea/`) project model.** For IntelliJ-native projects without a Gradle, Maven, or BSP model,
  the LSP statically reads `.idea/modules.xml`, every listed `*.iml`, `.idea/libraries/*.xml`, and
  `.idea/misc.xml`; no IDE, JVM, or build tool is launched. Detection order is `Explicit` > `BSP` >
  `Gradle`/`Maven` > `JPS` > `None`. JPS remains the fallback across the full bounded ancestor search, so
  a nested `.idea` model cannot hide a parent build-tool marker. Each `.iml` maps to a main module and,
  when it declares test roots or test-scoped dependencies, a test module. All `<content>` roots are
  scanned; generated roots are marked, resources are excluded, project and module library `CLASSES`
  roots form the classpath, and module order entries form dependency edges. `RUNTIME` entries are excluded
  from compile classpaths; `TEST` entries are visible only to the test module. The test module depends on
  its main module and receives the main output as a friend path.

  IntelliJ path macros (`$PROJECT_DIR$`, `$MODULE_DIR$`, `$MAVEN_REPOSITORY$`, `$USER_HOME$`) are expanded
  before `file:` and `jar:` URLs pass through the shared local-file URI decoder. Unknown macros and
  unavailable home-dependent macros are skipped instead of becoming relative paths. Project and module
  language levels become `jvm_target`; preview levels use their underlying JVM version. The project SDK
  name is matched against JetBrains `jdk.table.xml` files and accepted only when it resolves to a valid
  JDK home. Malformed or unreadable listed model files fail the probe, allowing transactional refresh to
  retain the last good model. JPS-only watcher globs are registered only while JPS is active, preventing
  IntelliJ metadata churn from retriggering Gradle or Maven. The shared XML reader now exposes element
  attributes for this attribute-driven format. Tests: `crates/krusty-lsp/src/project/jps.rs`,
  `project/detect.rs`, and the shared project-model test suite.

- **Primitive-bounded type parameters (specialization).** kotlinc specializes a type parameter with a
  primitive upper bound to that primitive — `fun <T: Int> f(t: T): T` compiles to descriptor `(I)I`, not
  `(Object)Object`. krusty specializes a FUNCTION type parameter whose bound is an INTEGRAL wrappable
  primitive (`Int`/`Long`/`Short`/`Byte`/`Char`/`Boolean`) via `TParams` (name → erasure `Ty`). NOT
  specialized (still rejected → the file skips, never miscompiles): CLASS type parameters (the value-class
  pass owns class-bound handling; naive specialization breaks the Object/value-class boundary →
  VerifyError), floating bounds (`Double`/`Float` — boxed-vs-primitive `==` differs on −0.0/NaN), and
  unsigned/value bounds. The generic `Signature` attribute is not emitted (a systemic krusty-generics
  gap), so byte-parity for generics is not yet achieved; runtime (box) is correct. Tests:
  `tests/primitive_bound_generic_e2e.rs`.

- **Reference-bounded type parameters erase to the bound (not `Object`).** kotlinc erases a bounded `T`
  to its bound's JVM type — `fun <T : CharSequence> f(x: T): T` has descriptor
  `(Ljava/lang/CharSequence;)Ljava/lang/CharSequence;`, not `(Object)Object`. krusty resolves the
  declared bound in `TParams::from_decl_with` (a class-name → JVM-internal resolver, `resolve.rs`) and
  stores it as the FUNCTION type parameter's erasure, so member/operator resolution on a `T`-typed value
  peels to the real bound and the descriptor uses it (`String`/user-class/Kotlin-builtin bounds; an
  unbounded `T` stays `Object`, a primitive bound still specializes). A CLASS type parameter erases the
  same way (`class Bounded<T : Cargo>(val t: T)` signs its constructor, backing field, and getter with
  `Lapp/Cargo;`): `TParams::erased_with` builds the class scope from the declared bounds and then
  collapses any NON-REFERENCE erasure back to `Any`, so a reference bound reaches the descriptor while a
  primitive bound keeps the erased model the value-class pass depends on. Enclosing declarations (an
  outer class of an `inner`, the declaration surrounding a local class) are folded in first, so an inner
  class erases the outer's bounded parameter too and a same-spelled own formal shadows it. The generic
  `Signature` attribute is still omitted (same gap as above). The bound is also visible to the JVM **mapped-builtin**
  member tables below, so `x.get(i)`/`x.toInt()`/`x.length` on a `<T : CharSequence>`/`<T : Number>`
  resolve. NOT supported: a `<T : Comparable<T>>` bound whose body uses the `<`/`>` operator AND is called
  with a primitive (`maxOf2(3, 5)`) — that needs the type argument inferred (`T = Int`) and the primitive
  BOXED into the `Comparable`-erased parameter slot, which krusty's emit does not do (a raw `int` reaching
  a `Comparable` parameter is a VerifyError), so such a call is DECLINED (the file skips), never
  miscompiled. One facet remains open, and it is shared by the function and class paths (so not a
  class/function asymmetry): a NULLABLE bound (`<T : Cargo?>`) still erases to `Object` where kotlinc
  uses `Lapp/Cargo;` — `tparam_bound_erasure` keeps `Any` for a nullable bound, deliberately.
  Tests: `tests/bounded_type_param_e2e.rs`, `tests/class_type_param_bound_erasure_e2e.rs`.

- **A type parameter with a NON-NULL bound is a non-null reference.** `<T : Cargo>` and `<T : Any>`
  cannot hold null, so kotlinc annotates the field, the getter, the constructor parameter and a `var`
  setter's parameter `@NotNull`, and guards `<init>`/the setter/a method parameter with
  `Intrinsics.checkNotNullParameter`; an unbounded `<T>` (implicitly `Any?`) or a `<T : Cargo?>` gets
  NEITHER — kotlinc leaves the nullable case UNANNOTATED rather than marking it `@Nullable`. This is
  independent of the erasure above: `<T : Any>` still erases to `Object` yet takes the annotations and
  the guard. Lowering reads the DECLARED bounds (`declared_type_param_admits_null`, following a bound
  that names a sibling parameter); the JVM emitter states the same rule over the RESOLVED bounds in the
  class's generic signature (`IrFile::class_type_param_admits_null`). One predicate
  (`field_nullability_kind`) serves the constant-pool seeder, the field/accessor/parameter annotations,
  the setter guard, and the constructor's `LineNumberTable` start pc — they must agree, since a field
  classified as guarded in one and unguarded in another puts the line entry at the wrong offset. With
  this, a bounded generic class is BYTE-IDENTICAL to kotlinc. Test:
  `tests/class_type_param_bound_erasure_e2e.rs`.

- **A mapped collection's member scope comes from `.kotlin_builtins`, not from the JVM class.** A mapped
  Kotlin type (`kotlin/collections/MutableList`, …) has no `.class` of its own; krusty resolves it through
  the JVM type it maps to (`java/util/List`). That class's method set is NOT its Kotlin API. `java.util.List`
  declares `remove(int)` (remove BY INDEX) alongside `remove(Object)` (remove the ELEMENT), plus `stream`,
  `toArray`, `getFirst`, `spliterator` — none of which Kotlin's `MutableList` has. Kotlin declares only
  `MutableCollection.remove(element: E): Boolean`; the index-taking method is reachable solely under the
  renamed name `removeAt` (kotlinc's `BuiltinMethodsWithDifferentJvmName`). Taking the Java set therefore
  MISCOMPILED: `list.remove(10)` bound the primitive-`int` overload — removing whichever element sits at
  index 10, or throwing `IndexOutOfBoundsException` — because an `Int` argument fits `I` exactly while
  `remove(Object)` needs boxing.

  So for a mapped COLLECTION the `.kotlin_builtins` declaration supplies BOTH the members and the
  supertypes, replacing the JVM class's rather than joining them — the supertypes too, or `java/util/List`
  re-enters one rung up the receiver walk and re-supplies everything. The class file still states the kind
  and constructors. Nothing physical changes: the builtins decode to the same erased descriptors and the
  same JVM owner, member names stay in SOURCE terms, and the Kotlin → JVM rename happens where it always
  did, at emit (`names::mapped_builtin_virtual_name`, `removeAt` → `remove`). No filter subtracts from the
  Java scope and no reverse table exists — the correct set is simply the declared one. The OVERRIDE
  direction is unchanged: a class realizing `MutableList` writes `removeAt`, and `mapped_interface_members`
  emits the `remove(int)` bridge. Tests: `tests/mapped_collection_scope_e2e.rs`, corpus
  `specialBuiltins/irrelevantRemoveAtOverride.kt`.

  A CONCRETE `java.util` class (`ArrayList`, `AbstractList`) is the other half, and needs the other
  mechanism: it has a real class file, so it never consults the builtins and keeps its Java member scope —
  including its own `remove(int)`. kotlinc handles exactly this in `LazyJavaClassMemberScope`
  (`isVisibleAsFunction` / `doesOverrideRenamedBuiltins` / `createRenamedCopy`): a Java method whose
  signature matches a renamed builtin is hidden under its JVM name and re-exposed under the Kotlin one.
  krusty derives this read-side rename from the same `mapped_interface_members` semantic handoff used
  for bridge emission. The selected mapping must match the JVM name AND full erased descriptor (only
  `remove(int)` is renamed, not `remove(Object)`) and its declaring mapped interface must occur in the
  concrete receiver's hierarchy. There is therefore no second reverse table to drift, and an unrelated
  class declaring `remove(index: Int): Any` is untouched. Verified against kotlinc:
  `arrayListOf(10, 20, 30).remove(10)` removes the ELEMENT on both `ArrayList` and `AbstractList`
  receivers, while `removeAt(0)` emits `remove(I)`.

  The COLLECTIONS **and `kotlin/String`**. `java.lang.String`'s method set had been leaking wholesale into
  the Kotlin scope — measured against kotlinc 2.4.10, 18 names it reports as unresolved (`getChars`,
  `concat`, `replaceAll`, `equalsIgnoreCase`, `compareToIgnoreCase`, `getBytes`, `strip*`, `transform`,
  `indent`, …). One of them miscompiled rather than merely over-accepting: `java.lang.String.split(String)`
  splits on a REGEX and returns `Array<String>`, so it shadowed Kotlin's literal-delimiter
  `CharSequence.split(vararg delimiters: String): List<String>` and `"abcdef".split("c")` produced the wrong
  type from the wrong semantics. Making the builtins authoritative closes all 18.

  Whether a mapped builtin's Kotlin declaration REPLACES or JOINS its JVM source scope is stored beside
  that builtin's centralized Kotlin↔JVM erasure identity. The classpath loader therefore consumes a
  semantic provenance property plus the fact that metadata was decoded; it does not reconstruct a
  collection-or-class-name exception branch. This keeps members and supertypes on one policy and gives
  future whitelist work one mapping table to change.

  Two things had to move with it. The three shapes the Java set had been covering — `substring(Int)`,
  `substring(Int, Int)`, `indexOf(String)` — are `kotlin.text` EXTENSIONS (an `@InlineOnly` splice down to
  the Java member, and `StringsKt.indexOf$default`), and the extension seam resolves all three; what stopped
  them was a hardcoded `rt == Ty::String` arm in the checker that typed them WITHOUT recording a call
  target. Sitting above the extension section it took over the moment the Java members went away, so the
  front end accepted the call and the IR lowerer bailed with "unrecorded qualified call target". It now sits
  BELOW that section, where it is only what it was always meant to be: a typing fallback for a
  CLASSPATH-FREE check, with no `StringsKt` to bind. Emitted bytecode matches kotlinc exactly —
  `substring` → `invokevirtual java/lang/String.substring`, `indexOf` → `invokestatic
  kotlin/text/StringsKt.indexOf$default`. Second, the authoritative test is the PRESENCE of the decoded
  `.kotlin_builtins` declaration, never a non-empty member or supertype vector — an authoritative
  declaration is allowed to state an empty set, and switching only half the shape would recreate the leak.
  Presence is also what keeps a classpath carrying a JDK but no kotlin-stdlib correct: nothing decodes
  there, so `String` keeps the JVM class's supertypes instead of being left with none (it would otherwise
  lose `CharSequence`, `Comparable` and `Any`, and every subtype test against them would fail).

  One supertype survives the replacement: `java/io/Serializable`. It is not a Kotlin type, so it appears in
  no `.kotlin_builtins` declaration — but kotlinc still reports a mapped builtin as implementing it whenever
  the Java class does, adding it back in `JvmBuiltInsCustomizer.getSupertypes` (`isSerializableInJava`).
  Dropping it made `val v: java.io.Serializable = "abc"` an error against a kotlinc that accepts it. The
  mapped COLLECTIONS never exposed this: `java/util/List` does not implement `Serializable`, and a concrete
  `java.util` class that does (`ArrayList`) is not an authoritative name. A member-name probe cannot see
  supertypes, so this needs its own coverage. Tests: `tests/mapped_string_scope_e2e.rs`.

  Mapped collection scopes also admit `jvm_class_map::MAPPED_VISIBLE_METHODS`, matching
  `JvmBuiltInsSignatures.VISIBLE_METHOD_SIGNATURES`. Read-only signatures such as `stream` and
  `getOrDefault` are visible on both collection faces; mutating signatures such as `removeIf`,
  `computeIfAbsent`, and `merge` require a `Mutable*` receiver. Tests:
  `tests/mapped_collection_scope_e2e.rs`.

  Other mapped built-ins retain their JVM scope. This keeps `CharSequence.chars`, `Enum.name`, and the
  visible `Throwable` methods available. `kotlin/Throwable` still exposes Java `getCause` and
  `getMessage` in addition to the Kotlin properties. The inherited `String` scope is JDK-dependent, so
  negative String-scope tests use members declared only by `java.lang.String`, not members that may be
  added to `java.lang.CharSequence`.

- **Kotlin members on JVM-mapped built-ins (`CharSequence`/`Number`/`Comparable`).** kotlinc maps these
  Kotlin types to JVM classes (`java/lang/CharSequence`, …) but their Kotlin API differs from the JVM
  class's methods — `CharSequence.get(i)` dispatches to `charAt`, `Number.toInt()` to `intValue`, and the
  `length`/`get` members live in `.kotlin_builtins`, not on the `.class`. krusty resolves such a member
  from the builtins metadata keyed on the Kotlin name (`jvm_to_kotlin_builtin_with_members` maps
  `java/lang/CharSequence` → `kotlin/CharSequence`) when the classpath `resolve_instance` can't, and the
  backend emits the call via `Classpath::builtin_member_call` — which maps the owner to its JVM class,
  carries the renamed JVM method name (`get` → `charAt`, `toInt` → `intValue`; the rename table mirrors
  kotlinc's `BuiltInMethodsWithDifferentJvmName`), and reports interface-ness for the correct
  `invokeinterface`/`invokevirtual`. The codegen path fires ONLY for a RENAMED member; a same-named member
  (`compareTo`, `length`) is left to `resolve_instance` so a real (e.g. value-class) receiver dispatches
  correctly. Tests: `tests/bounded_type_param_e2e.rs`.

- **Unchecked cast to a type parameter (`x as T`).** kotlinc erases the target to the type parameter's
  upper bound — `Object` for an unbounded `<T>` (no `checkcast` emitted), the bound's class for `<T :
  CharSequence>` (a `checkcast`). A non-null bound (`<T : Any>`, `<T : Foo>`) null-checks first
  (`Intrinsics.checkNotNull`, throwing on `null`); an unbounded `<T>` (= `<T : Any?>`) does not. krusty
  keeps `T` (with its bound) in the IR as `IrType::TypeParameter { name, bound }` and erases it ONLY at
  emit (`ir_ty_to_jvm` collapses it to the bound; the `Object` case emits no `checkcast`) — the type
  system never erases. A generic call whose result is a bare `T` is refined at the call site to the
  supplied type argument (a primitive arg → its boxed wrapper, the erased slot's real representation),
  with the `checkcast` kotlinc inserts on the result. Cases needing a coercion krusty doesn't model — a
  `<Unit>`/`<Nothing>` argument, an erased generic call inside an `inline` expansion, or the
  `-Xbinary=genericSafeCasts` flag — skip the file rather than miscompile. Tests:
  `tests/typeparam_cast_e2e.rs`.

- **Cast to a nullable reference type (`x as Foo?`).** A plain `checkcast Foo` — the JVM `checkcast`
  passes `null` through, so `null as Foo?` is `null` (never a throw) and a wrong non-null type throws
  `ClassCastException`; contrast `x as Foo`, which null-checks first (`CastNonNull`). The cast target is
  resolved by its non-null form (a nullable reference and its non-null form share the JVM class); only
  the null-throwing behaviour differs. A nullable VALUE-class target (`as Str?`) is excluded — it stays
  boxed, and the value-class pass would unbox a `null` (NPE) — so it skips rather than miscompile. Test:
  `tests/nullable_cast_e2e.rs`.

- **Generic higher-order method (`class Box<T> { fun <R> map(f: (T) -> R): R }`).** A call on a
  parameterized receiver substitutes BOTH the receiver's type arguments and the method's own type
  parameter. The lambda parameter `it` types as the receiver's element type (`Box<String>.map { it…}` →
  `it: String`), recovered like the class-type-parameter property substitution — not the erased `Object`.
  The method type parameter `R` is inferred from the lambda body's type (`{ it.length }` → `Int`) and
  becomes the call's result type — the source-`TypeRef` analogue of the library `GSig` unify/substitute
  machinery (`unify_ref`/`ty_of_ref` over a `GenericMethod` shape stored on `ClassSig`, populated at
  collection because `TypeRef` is owned/file-independent). The JVM method still erases `<R>` to `Object`;
  the checker recovers the concrete result so codegen inserts the `checkcast`/unbox kotlinc emits on the
  erased return (`coerce_generic_read` now also wraps a user instance-method call). Covers a reference
  element type (`Box<String>`, `it.length`) and a primitive one (`Box<Int>`, `it * 2`), with `R` inferred
  to both a primitive and a reference. Constructor argument-based type inference is unmodeled, so the
  receiver's type argument comes from the declared variable type (`val b: Box<String> = Box("hi")`), as
  with the property-substitution path. Tests: `tests/generic_hof_method_check.rs` (front-end) and
  `tests/generic_fn_e2e.rs::generic_hof_method_substitution_runs` (round-trip).

- **Interface delegation to an expression (`class D : I by Impl()`).** A delegate that is not a `val`
  constructor parameter but an arbitrary EXPRESSION: it is evaluated once into a synthesized
  `$$delegate_e<j>` field (stored in the constructor, with ctor params and `this` in scope, so
  `by mk(x)` works), and each of `I`'s methods forwards to it. The `{` after `by Impl()` opens the
  CLASS BODY, never a trailing lambda on the delegate call. Skips (never miscompiles): a VALUE-class
  delegate (unboxed → doesn't implement `I` at runtime), and — as for the existing non-`val`-param
  path — a generic or property-bearing interface. A separate fix: `file_class_name` sanitizes
  characters illegal in a JVM class name (`foo.1.0.kt` → `Foo_1_0Kt`, not a `ClassFormatError`). Test:
  `tests/interface_delegation_expr_e2e.rs`.

- **Delegation forwarder ORDER is the delegated interface's declaration order.** kotlinc emits one
  forwarder per delegated member, in the order the interface declares them; krusty matches. The
  member set is read out of the semantic symbol table, which keys members by source name in a hash
  map, so each interface's contribution is ordered by the declaration coordinate (`file`, owner,
  member index) its signature carries before any forwarder is synthesized. Members with no AST
  coordinate sort last, by name. Without this the emission order — and with it constant-pool intern
  order and the emitted bytes — varied with the process's hash seed: the same binary alternated
  between two byte-different classes for corpus
  `multiplatform/k2/delegation/delegationToExpectInterface_withNewMembers`, defeating byte-for-byte
  reproducibility and adding false positives to class-byte sweeps. Super-interface contributions keep
  their existing breadth-first grouping; ordering applies within each interface. Tests:
  `tests/interface_delegation_e2e.rs::forwarders_follow_interface_declaration_order`,
  `…::forwarder_emission_is_byte_deterministic`.

- **Property with a backing field + custom accessor referencing `field`.** `val x = "O" get() = field
  + "K"` / `var v = 1 get() = field + 10 set(value) { field = value * 2 }` — a stored backing field
  AND a custom getter/setter (distinct from a computed property, which has no field, and a plain field,
  which has default accessors). The backing field is emitted with its initializer; the synthesized
  `getX`/`setX` run the custom accessor body, with `field` bound to that backing field (read →
  `GetField`, write → `SetField`). Crucially, EVERY access to the property — even in-class, including
  `x`, `x = …`, `x += …`, `x++` — routes through `getX`/`setX`, never the raw field (`resolve_field`
  and the direct unqualified read/write/incdec sites all decline a custom-accessor property); only the
  `field` keyword inside the accessor reaches the field. Tests: `tests/backing_field_accessor_e2e.rs`.

- **Top-level property with a backing field + custom accessor.** `val x = "OK" get() = field`,
  `var v = 0 set(value) { field = value }` at file scope. The backing field is a facade STATIC
  (initialized in `<clinit>`); the synthesized `getX`/`setX` are emitted as ordinary facade static
  methods running the custom body, with `field` bound to that static (read → `GetStatic`, write →
  `SetStatic` — the static analogue of the member `cur_field` path). A default accessor is synthesized
  when only one side is custom (`var v = 0 set(...)` still gets `getV` = `return field`). Same-file
  reads route through `getX` (via `computed_props`) and writes through `setX` (via `computed_setters`),
  never the raw `putstatic`, so a custom getter's logic always runs — byte-identical to kotlinc's
  `getstatic;areturn` getter + `<clinit>` store. The trivial auto-accessor is suppressed
  (`IrStatic::custom_accessor`) to avoid a duplicate-method collision. Tests:
  `tests/top_level_custom_accessor_e2e.rs`.

- **`lateinit var` LOCAL.** `lateinit var s: String` in a function body — a mutable slot with no
  initializer, defaulting to `null` (`aconst_null; astore`); a read while still null throws
  `UninitializedPropertyAccessException`. Parsed as `Stmt::LocalLateinit` (distinct from `Stmt::Local`,
  whose initializer is mandatory) and only for a non-null reference annotation (a primitive/nullable/
  unresolved type bails). Each read is wrapped in an `IrExpr::LateinitCheck` — the same guard the
  member-field lateinit read uses (`dup; ifnonnull L; ldc name;
  invokestatic Intrinsics.throwUninitializedPropertyAccessException; L:`). This is behaviorally exact
  for every access; kotlinc additionally omits the guard where definite-assignment analysis proves the
  slot is initialized (a plain read) or unset (an unconditional throw), so krusty's always-guarded read
  is byte-identical only for a maybe-initialized read (byte-parity for the DA-optimized cases is future
  work). A CAPTURED (shared-cell) lateinit local is not modeled — its slot is a `Ref` box whose read
  path carries no guard — so such a file bails (skip, never miscompile). Tests:
  `tests/lateinit_local_e2e.rs`.

- **Cast of a primitive operand to a reference type (`42 as Any`, `'a' as Char?`, `b as Byte?`).** A
  boxing operation — the primitive is boxed to its wrapper (`Integer`/`Character`/`Byte`, an
  `ImplicitCoercion` → `valueOf`), which is-a the target. Allowed ONLY when the wrapper is assignable
  to the target (`Any`/`Object`, the wrapper itself, or a supertype like `Number`/`Comparable`); an
  impossible cast (`1 as String`) is rejected, not boxed — boxing an `Integer` into a `String` slot is
  a load-time VerifyError, and kotlinc rejects it at compile time anyway. A type-parameter target
  (`56 as T`) is excluded: the boxed value would flow into an erased/bridged generic slot krusty does
  not reconcile (it skips). Unsigned operands (`1u as Any`) are excluded too. Test:
  `tests/primitive_box_cast_e2e.rs`.

- **Named arguments on a constructor call (`C(b = 9)`).** The primary constructor's parameter names map
  the labels onto positions, exactly as for a top-level function — including a call that skips a leading
  parameter whose default is a simple literal (the checker maps via `map_call_args`, the lowering fills
  the default). A named call references the PRIMARY constructor's parameter names only; it is NEVER routed
  to a same-arity secondary constructor that merely coincides on argument types (the secondary-selection
  paths are gated on the call being positional — otherwise `C(b = 9)` against a `constructor(x: Int) :
  this(x, x)` would set `a` instead of using its default → wrong fields). An omitted parameter with a
  non-literal default skips at lowering. Tests: `tests/named_ctor_args_e2e.rs`.

- **Method type parameter that shadows its class's (`class Box<T> { fun <T> m(x: T): T }`).** The
  classpath member-return substitution (`JvmLibraries::member_return`) binds a generic class's formal
  type parameters to the receiver's type arguments (`Box<String>` → `{T: String}`) and substitutes a
  member's generic return under them, so `List<Int>.get(i): E` types as `Int`. A method that declares
  its OWN type parameter of the same name is INDEPENDENT of the receiver's argument — the substitution
  now drops every class binding whose name the method re-declares (recovered from the method's generic
  signature, already parsed), so the shadowing `T` erases to its bound instead of mis-binding to the
  receiver's argument. Without this, `Box<String>.m(42)` typed as `String` and the call site would
  `checkcast String` an `Integer` → `ClassCastException`. Kotlin warns on such shadowing, so it is
  absent from the same-file box corpus; the same-file member path does no such substitution (a generic
  member return is left at its erased bound), so the bug is classpath-only. Test:
  `tests/shadowed_method_tparam_e2e.rs` (a `javac`-compiled generic class with a shadowing method).

- **Member resolution through INTERFACE supertypes read from classpath `@Metadata`.** A call on a
  receiver whose declared type is a classpath interface resolves members inherited from its
  super-interfaces: the member walk (`JvmLibraries::functions`, receiver branch) is breadth-first over the
  receiver's supertype closure (`ConfigRepo : CrudRepo, Named` inherits `save`/`findById`/`id`). Three
  entangled shapes are covered:
  - **Function-typed parameter members** (`Logger.info(msg: () -> Any?)`). The classpath decodes a
    function-type parameter as a `Ty::Fun`, so a lambda argument (also `Ty::Fun`, but with a different
    return type) never paired under plain equality / `Any` widening. `best_member_overload`
    (`call_resolver`) now matches a `Ty::Fun` argument to a function-typed parameter (a decoded `Ty::Fun`
    OR an erased `kotlin/jvm/functions/FunctionN`) by ARITY — the lambda body adapts its return.
  - **`suspend` interface members** (`suspend fun getConfig(id): Config`). The member walk strips the
    trailing `Continuation` parameter and recovers the real return from the `Continuation<T>` type
    argument in the generic signature (`suspend_return_from_gsig`; `Continuation<-Unit>` → `Unit`). Member
    suspend detection reads both the file facade's `Package.function` (field 3) and a class/interface's
    `Class.function` (field 9) `MetaFn::is_suspend` flag — it previously saw only top-level functions, so
    interface/class member `suspend` funs were invisible.
  - **Lowering a classpath suspend-member call.** A `LibraryMember` now carries `suspend`; the classpath
    instance-call lowering records the call in `ir.suspend_calls` so the coroutine pass threads the
    `Continuation` (its CPS descriptor rebuilt for a `Callee::Virtual` in `append_continuation`) and types
    the resumed result. The resume value (erased `Object`) is `checkcast` to a concrete reference return
    (`unbox` in `jvm::suspend` now emits `Cast` for a reference target, but NOT for a boxed-primitive
    object type such as `Obj("kotlin/Int")`, where `ImplicitCoercion` must UNBOX to the JVM primitive).
  Tests: `tests/interface_supertype_members_e2e.rs` (a kotlinc-built interface library; krusty compiles a
  caller that inherits CRUD members from a super-interface, binds a lambda to `Logger.info`, and drives a
  `suspend` inherited member through a Java `Continuation` — both round-trip on the JVM).

- **Concrete generic return of a classpath member keeps its type argument.** `member_return`
  (`JvmLibraries`) propagates only the RECEIVER's own type arguments, so a member on a NON-generic
  receiver whose return is a concrete generic (`class Repo { fun all(): List<Item> }`) fell back to the
  erased `List` — its element then typed as `Any`, and `r.all().forEach { it.id }` / `.map`/`.first()`/
  `[0]` all failed with "unresolved member on `kotlin/Any`". The member walk now recovers a FULLY CONCRETE
  generic return (`concrete_generic_ret`: the return's generic signature carries type arguments, none a
  free type variable) as `List<Item>`, so element access / lambda parameters / `first()` type as `Item`.
  A return naming a type variable (`fun <T> load(): T`, `List<E>.get(): E`) is untouched — it stays erased
  or is bound by `member_return` under the receiver's arguments. Test:
  `tests/interface_supertype_members_e2e.rs::concrete_generic_return_keeps_type_argument`.

- **Class literals bind Java class-token APIs through nested generic returns.** `C::class` carries
  `KClass<C>` during both signature inference and checking. Metadata specializes
  `KClass<T>.java: Class<T>`, so Java method type parameters bind from `Class<C>` and substitute through
  nested return types. Receiver-owned type parameters remain bound when a later member receives `null`;
  method-owned parameters still bind from call arguments. Test:
  `classpath_static_call_inference_e2e::class_literal_binds_nested_java_generic_returns`.

- **An unbound class literal on an ARRAY type resolves its spelling as a type, arguments included.**
  `Array<String>::class` / `IntArray::class.java` resolve through the ordinary typeref channel with the
  type arguments the parser attached to the reference node, so the represented type is
  `Array<String>` / `IntArray` — the element type is part of the JVM class constant
  (`[Ljava/lang/String;`, `[I`). A bare name that binds a value stays a bound literal; a type parameter
  stays on the reified channel. Tests: `class_literal_e2e::array_class_literals`,
  `class_literal_e2e::array_class_literals_report_no_diagnostic`.

- **Signature inference binds a callee type parameter from a lambda argument's RESULT.** When a type
  parameter occurs only in the lambda's return position (`lazy { 1 }`, `make { 1 }`,
  `listOf("x").map { it.length }`), the light signature pass infers the lambda body under the
  substituted shape and unifies the result, so the property/return reads back the bound type instead
  of `Any`. Selection uses the same candidate entry points as checking (`select_call_template`), and
  the template result is used only when a lambda result actually contributed a binding. Tests:
  `tests/lambda_result_inference_e2e.rs`.

- **A delegated property's getter coerces the physical `getValue` result to the checked property
  type.** Lowering consumes the property type recorded by the checker. The backend unboxes a
  primitive result or checkcasts a narrower reference result at the accessor boundary. Tests:
  `lambda_result_inference_e2e`, `delegated_prop_e2e`.

- **String-template interpolation allows line breaks around the expression.** `"${" NL* expression
  NL* "}"` per the Kotlin grammar — a multiline lambda inside `${…}` (common in raw strings) parses.
  Plain line breaks only; an explicit `;` still terminates the expression. Test:
  `nested_string_template_e2e::template_interpolation_allows_newlines_around_expression`.

- **A Java instance field is a Kotlin `var` property (unless `final`), with Java visibility.** Public
  fields read/write anywhere; protected fields bind from a subclass of the declaring class (any
  depth). An accessible field beats a bean-getter synthetic property even across hierarchy rungs
  (kotlinc emits `getfield`/`putfield`, not the getter); an inaccessible field falls back to the
  synthetic property (getter call on read, `'val' cannot be reassigned.` on write). The resolver walk
  returns the field with its visibility/finality beside the tentative synthetic property
  (`PropertyInfo::synthetic`), the checker accepts visibility at the site, and lowering consumes the
  recorded `InstanceFieldRef` on `IrExpr::PropertyWrite`. Tests:
  `java_source_interop_e2e::java_instance_field_writes_public_and_protected`,
  `java_source_interop_e2e::java_instance_field_write_rejections_match_kotlinc`.

- **A lambda argument to a Java STATIC method's SAM parameter carries the call's implicit return
  label.** `SwingUtilities.invokeLater { … return@invokeLater }` binds the label to the lambda
  exactly like the instance-method and Kotlin top-level paths: `provider_member_lambda_arg_kinds`
  threads the callee name into `check_lambda_with_expectation` for both classifier call shapes
  (value-facet and static-namespace) instead of dropping it. Test:
  `sam_classpath_e2e::java_static_sam_lambda_return_label_runs`.

- **Java package-private members are visible from Kotlin in the SAME package.** `Visibility` gained
  `PackagePrivate` (a Java class-file-only fact); the classfile decoder retains package-private
  methods/fields instead of dropping or erasing them, and the single `member_accessible` gate admits
  them iff the current file's package equals the owner's package — statics, instance methods, fields,
  and constructors all flow through that one arm. Cross-package access reports kotlinc's exact pair:
  `cannot access 'class Helper : Any': it is package-private in file.` at the qualifier and
  `cannot access 'static fun adjust(): Unit': it is package-private in 'p.Helper'.` at the callee.
  Tests: `java_source_interop_e2e::package_private_java_static_callable_within_same_package`,
  `java_source_interop_e2e::package_private_java_static_rejected_cross_package`.
  An INACCESSIBLE package-private candidate never shadows an accessible one: the
  `select_member_property` walk declines to bind a package-private field the current file's package
  cannot read (one `package_private_member_accessible` check beside the existing private/static
  hides-without-binding cases), so `HashMap.size`'s package-private field no longer hides the public
  `size()` property facet from non-`java.util` code — the `cannot access` diagnostic fires only when
  the package-private declaration is the sole candidate. Tests:
  `map_entry_destructure_e2e::discarded_map_put_does_not_unbox_null`,
  `ir_lower_deep_coverage_e2e::map_index_get_set`.

- **A package-private static FIELD of a public Java class follows the same rule.** The classpath
  `static_field_name` decoder retains package-private static fields (only `private` is dropped) and
  `StaticFieldRef` carries the declared `visibility`; the three checker read sites (qualified
  classifier read, member-read fallback, `read_classifier_member`) all pass through the one
  `record_static_field_gated` helper — same-package reads bind and emit `getstatic`, cross-package
  reads report `cannot access 'static field count: Int': it is package-private in 'p.Pub'.` at the
  member segment. Tests:
  `java_source_interop_e2e::package_private_java_static_field_read_within_same_package`,
  `java_source_interop_e2e::package_private_java_static_field_rejected_cross_package`.

- **Signature inference binds a callee type parameter from a lambda argument's RESULT.** When a type
  parameter occurs only in the lambda's return position (`lazy { 1 }`, `make { 1 }`), signature
  inference checks the lambda under the substituted parameter shape and unifies its result. The
  inferred property type therefore retains the binding. Test: `tests/lambda_result_inference_e2e.rs`.

- **Generic classpath extension properties retain Kotlin return semantics.** The metadata decoder
  preserves property formals, receiver, return type, bounds, and nullability. Resolution specializes
  that logical type from the receiver, while lowering bridges the erased getter result. Test:
  `classpath_static_call_inference_e2e::generic_extension_property_keeps_nullability_and_kotlin_collection_type`.

- **`@JvmStatic` member of a classpath `object` (`IdGen.of(x)`).** kotlinc emits it as a static
  method on the object class, so it lands in the type's `companion` (static) list, NOT as an instance
  member — a call on the object value previously failed as "unresolved method on `<object>`". Both the
  checker (member-call fallthrough) and lowerer now try `resolve_companion` on the receiver's type and,
  when it matches, resolve/emit an `invokestatic` on the object class (the instance receiver is dropped,
  as kotlinc does). Test: `tests/interface_supertype_members_e2e.rs::jvmstatic_object_member`.

- **An OBJECT is a legal parent of a callable name, not just a package.** `import
  kotlin.time.Duration.Companion.minutes` did not resolve, so `10.minutes` was `unresolved reference`.
  Kotlin's rule is that importing a member of an object brings that name into scope WITH the object as
  its implicit dispatch receiver; for a member EXTENSION the use site supplies the extension receiver
  and the singleton is the dispatch. krusty's callable namespace is keyed by fully-qualified name, and
  `resolve_symbols_name` only ever read the parent of that name as a PACKAGE (`package_facades_name`),
  so an object or companion parent surfaced nothing — the one shape that worked,
  `import Obj.memberFun`, did so through a separate special case rather than the namespace.
  `object_member_extensions` now contributes the owner's member extensions as ordinary extension
  callables, so SELECTION is unchanged; only the emit differs, and that difference rides on
  `LibraryCallable::singleton_dispatch`. Three facts the shape forces:
  a companion is NOT `TypeKind::Object` (it has no `INSTANCE`; its singleton is a field on the OUTER
  class, named after the companion), so object-ness is decided by finding that field, and the field
  itself travels on the callable rather than being re-derived from a guessed name at emit;
  an import path spells every segment alike (`…/Duration/Companion`) while a nested class uses `$`, and
  which trailing segments are nesting is not knowable from the path, so split points are tried
  outward-in;
  and an `@InlineOnly` accessor (`Duration.Companion`'s are `private` in the class file) has no callable
  form at all, so a non-public accessor is surfaced as `MustInline` and emitted as a splice with the
  singleton bound as receiver instead of an invoke. Tests:
  `tests/classpath_object_member_extension_import_e2e.rs`.

- **A VALUE CLASS passed to a classpath TOP-LEVEL function resolves against its DECLARED type, not its
  erasure.** `taggedOnly(Tag("x"))` was `unresolved function`; `spend(budget = …)` was `argument type
  mismatch: actual type is 'lib.Budget', but 'Long' was expected`. A `@JvmInline value class` erases to
  its underlying in the descriptor (`Budget(val millis: Long)` → `J`, `Tag(val v: String)` →
  `Ljava/lang/String;`) while `@Metadata` names the class, and the erased form leaked into two places
  that must decide against the Kotlin type. (1) `top_level_overloads` published the DESCRIPTOR's
  parameter types, so selection compared a `Budget` argument against `Long`; the declared types are now
  restored from `@Metadata` (`MetadataCallFacts::value_class_params`) — LAST, after every
  metadata/bytecode alignment has matched the erased form the class file actually spells. The emit
  descriptor stays physical and the value-classes pass unboxes at the call, exactly as a mangled MEMBER
  with a value-class parameter is already exposed. (2) `meta_param_compat` / `meta_param_exact` decided
  the value-class case in the FINAL arm of an `else if` chain, so a value class with a REFERENCE
  underlying was judged by the arm for its erasure (`Ty::String` asks only whether the metadata name IS
  `String`) and rejected before reaching it — costing such a function its metadata alignment outright,
  parameter names and defaults included, which is why even a call passing NO value-class argument
  failed. Both now decide it up front. Test: `tests/classpath_value_class_param_e2e.rs` (both
  underlying kinds; members/constructors stay covered by `classpath_value_class_default_e2e`).

- **A TOP-LEVEL classpath `inline fun <reified T>` splices, and a body that cannot splice BAILS.**
  `nameOf<Svc>()` compiled clean and then threw `UnsupportedOperationException: This function has a
  reified type parameter…` — kotlinc's compiled body for a reified inline exists only to throw, so a
  direct call is never a legal fallback. The splice machinery was already correct; its INPUT was
  missing at three points, each a separate defect. (1) `reified_call_subst_for` — which pairs the
  callee's formal type-parameter names with the call's type arguments — was invoked only on the two
  EXTENSION lowering paths, and the checker recorded `resolved_call_type_args` only for extension and
  source calls, so a top-level call had no substitution and `splice_unified` refused to specialize.
  Both now cover the top-level arm. (2) A `$default` synthetic carries no generic `Signature`, so even
  with type arguments the formal NAMES were unknown; `resolve_top_level_default_callable` now
  propagates the BASE overload's signature onto the synthetic — the same reasoning already applied to
  `base_gsig`, and sound because the mask/marker parameters introduce no type variables. (3)
  `try_inline_static_as` declined every `$default` body outright; that retreat is only safe when a
  direct call is legal, so it now applies to non-reified callees only. The guard meant to catch this
  class of miscompile (`ir_emit`: bail rather than fall back) was itself keyed on the absent
  substitution, which is why a wrong program compiled silently.
  **Not spliceable, and refused rather than approximated:** a body calling
  `Intrinsics.needClassReification` (kotlinc's marker for "this materializes a class whose shape
  depends on `T` — regenerate it per call site", emitted for e.g. a default lambda typed on `T`).
  krusty splices instructions and does not regenerate a dependency's compiled inner classes, so
  `splice_unified` returns `None` and the backend reports an inline-splice error. `mockk<T>(…)` is
  this shape. Tests: `tests/classpath_reified_inline_toplevel_e2e.rs`.

- **A named argument binds by LABEL, including when it skips a defaulted parameter.** A classpath call
  that names a parameter and omits an earlier one (`mockk(relaxed = true)`, `runTest(timeout = …)`) was
  reported as `unresolved function`. The label→slot mapping was computed and then discarded: the
  arguments were compacted into a dense list and matched against the LEADING parameters, so the call
  resolved only when the supplied types happened to be assignable at those positions — `f(a: Int = 1,
  b: Int = 2)` called as `f(b = 5)` "worked" while `f(a: Int = 1, b: String = "z")` called as
  `f(b = "x")` did not, which is why the failure looked type-dependent and arbitrary. Selection
  (`symbol_resolver::resolve_top_level_named_default_callable` → `named_default_arg_mapping`) and the
  checker's argument check now both use the parameter slot the label names, and every unfilled slot
  must be defaulted for the `$default` synthetic to be applicable — with one documented exception: an
  EMPTY `param_defaults` means the provider recorded no default facts at all, which is read as
  "unknown, do not reject" exactly as `has_known_required_param` does, rather than as "nothing is
  defaulted". A callable with context parameters is declined outright, since the slots are
  value-parameter-relative while the parameter list is not. Lowering masks exactly the unfilled
  slots — EXCEPT a vararg: `$default` passes the array straight through and never fills it, so an
  omitted vararg is an EMPTY array with its mask bit CLEAR (`lower_default_slot_args` /
  `default_masked_slots`); masking it reached the callee as `null` and tripped its non-null parameter
  check at runtime.
  The TRAILING LAMBDA is shaped from its slot the same way. A lambda literal is typed BEFORE overload
  resolution, from the callee's block parameter — that is what gives it its receiver and arity — and
  `top_level_lambda_shape_in_scope` mapped arguments positionally, so `f(budget = 3) { }` aligned the
  `Int` against parameter 0, judged every overload inapplicable, and left the literal a bare
  `() -> Unit` that then failed against the erased `FunctionN`. It now maps through
  `call_argument_parameter_indices` — the same full Kotlin mapping the argument path uses, so labels,
  defaults, vararg, AND the trailing-lambda rule (an unlabelled `{ … }` binds the LAST parameter, not
  the next position) agree; `named_argument_map` alone does NOT encode that last rule, and using it
  here bound the lambda to the parameter after the labelled one. Exact-arity narrowing is skipped for
  a labelled call, whose argument count says nothing about which parameters are filled. All lambda
  kinds were affected identically — plain, receiver, `suspend`, `suspend` receiver — which is why the
  failure looked specific to `suspend` receivers. Test:
  `tests/classpath_named_arg_skips_default_e2e.rs`.

- **A property read is a property read; how it is READ is the target's business.** `Dispatchers.IO` was
  reported as `unresolved reference 'IO'`, and the cause was a category error rather than a missing case:
  the use denotes a Kotlin property, not a JVM accessor call — `getIO()` is only one possible class-file
  realization — yet that method spelling was carried all the way into resolution and lowering, and a read
  that could not be expressed as a zero-arg MEMBER METHOD therefore failed to resolve at all. `@JvmStatic`
  (which `Dispatchers` puts on every
  member) is an annotation for the JVM emitter: it moves the accessor off the singleton to a static of
  the object class, so the accessor is not an instance member and the lookup found nothing.
  The model now stops at the declaration. Resolution answers only what it owns — the receiver declares a
  property of this name (`SymbolResolver::member_property_type`), recorded as
  `ExprLowering::MemberPropertyRead` so lowering never re-decides what the member is — and lowering emits
  one node, `IrExpr::PropertyRead { receiver, owner, name, ty, interface }`, the same whatever the owner
  (this file, a sibling file, the classpath) and whatever the receiver. `ty` is the front end's answer for
  the read's Kotlin type, after substituting the receiver's type arguments; `interface` is declaration
  shape required for virtual dispatch when a streaming backend has not emitted the sibling source class
  yet. Neither field selects a target realization. Members still beat extensions, which matters here:
  `kotlinx.coroutines` also ships a binary-compat `DispatchersKt.getIO(Dispatchers)` EXTENSION property of
  the same name.
  The JVM backend decides the rest, and is the only layer that knows what `@JvmStatic` means.
  `Classpath::property_read_access` reads the owner's declaration for the realization — `@Metadata`'s
  `JvmPropertySignature` for a Kotlin class (so a `@JvmName` or value-class-mangled accessor is honoured,
  never guessed), a mapped-builtin/bean accessor or public field for a Java one — and `ir_emit`'s
  `emit_property_read` emits `getfield`/`getstatic`/`invokevirtual`/`invokeinterface`/`invokestatic`,
  bridging the physical result to the logical type (box, unbox, narrow). A realization that takes no
  receiver still evaluates one: a bare singleton or local read is elided, anything that can have an EFFECT
  is evaluated and popped — byte-for-byte kotlinc for `Cfg.p`, `local.p` and `side().p`. A value class's
  sole property is its erased underlying, so `value_classes` rewrites that read to identity rather than
  any accessor.
  Writes are the same shape: `IrExpr::PropertyWrite`, recorded by the checker as
  `StmtLowering::MemberPropertyWrite`, with `Classpath::property_write_access` /
  `declared_property_write_access` choosing the store (a field write inside the declaring class, the setter
  outside, and always the setter for a property with no backing field — a custom setter, a delegated
  `x$delegate`). This is what fixed the `@JvmStatic var` write, which emitted `invokevirtual` on the
  singleton and died at run time with `IncompatibleClassChangeError` — a miscompile, not a diagnostic.
  A property of a class this compilation declares goes through the same node — `GetField`/`SetField` are
  left to what they should mean, storage that is NOT a Kotlin property (coroutine state-machine slots,
  captured values, constructor field init, synthesized data/value-class members). The backend picks the
  direct field load only where it is legal, inside the declaring class, and reads the accessor's
  descriptor off the accessor itself: an accessor may return what the field's declared type does not
  spell, so a descriptor built from the field is a `NoSuchMethodError`, and a value-class-typed
  property's accessor is `@JvmName`-mangled (`getId-<hash>`), so missing that spelling falls through to a
  private field — an `IllegalAccessError`. A `Unit` property is stored as `Lkotlin/Unit;` but read
  through a `()V` accessor, so what the read leaves on the stack comes from the chosen realization
  (`descriptor_ret_words`), not from the declared type.
  A sibling source class likewise does not get a special common-IR branch. Its classfile is unavailable
  while another file streams through the backend, so the JVM value-class pass records an accessor's
  mangled JVM spelling in a JVM-only side table before erasure; the emitter consults that spelling only
  as its declaration-less fallback. The semantic node continues to name the Kotlin property.
  Default accessor synthesis also preserves declaration visibility: a `private set` remains private in
  both frontend access checking and the synthesized JVM method flags.
  The node carries the property's DECLARED type: substituting it to the type the site sees stays in the
  IR as before, because a pass that rewrites the read away still needs that bridging. And nothing ever
  narrows to a value class — it has no runtime type of its own, its values ARE the erased underlying — in
  the receiver narrowing or in the backend's physical-to-logical bridge.
  On JavaScript, a plain/default property realizes as a native field operation. A source-written getter
  or setter is retained as an IR function, however, so the JS emitter invokes that function; bypassing it
  with an unconditional field read/write would erase computed and custom-accessor behavior.
  The cost of a realization-shaped IR is paid by every pass that pattern-matches one, and each had to be
  taught the node: `suspend` walks it structurally, `ir_emit` tracks stack frames per node kind, and
  `value_classes` recognizes it in five places (the sole-property read that is the erased underlying, the
  plain-field getter identified by the read in its body, `constructor-impl`'s init inlining, and the
  nullability/boxing analyses) while erasing the type a WRITE carries.

- **A property's ACCESSORS are synthesized by the backend, not by lowering.** `getA()` is a realization of
  `val a`, so `ir_lower` records the declaration (`IrClass::properties`, an `IrProperty` per declared
  property: type, backing-field index, visibility, modality, and the lowered BODY of a source-written
  accessor) and `ir_emit::emit_declared_property_accessors` emits the method — its name, descriptor,
  dispatch, `getfield`/`putfield` body, generic `Signature`, and the `checkNotNullParameter` guard kotlinc
  puts on a non-null reference setter. Only a source-written accessor (computed, delegated, `field`-using)
  is lowered as a method, because only its body is Kotlin. Details that bit, all now driven off the
  declaration: the accessor descriptor comes from the ACCESSOR, never the field (an accessor may return
  what the field's type does not spell); a `Unit` property is stored as `Lkotlin/Unit;` but read through a
  `()V` accessor, so its stack effect comes from `descriptor_ret_words`; a value-class-typed property's
  accessor is `@JvmName`-mangled, and an OVERRIDE of one keeps the plain spelling while its BRIDGE takes
  the supertype's mangled name — read from the supertype's actual accessor, never a recomputed hash; and a
  class of this compilation is always answered from its declaration, never from the naming-convention
  fallback, which has no class file and would mistake an interface for a class.

- **A private property reached from outside its class gets kotlinc's `access$get<X>$p` bridge.** An
  `inline` body is spliced into its caller, where the private backing field is unreachable. krusty used to
  decline the read, which made the splice bail and emit an ordinary call — silently turning an `inline`
  call into a non-inline one, a different program. `IrProperty::needs_access_bridge` records the need
  during lowering and the backend emits the synthetic static, so the splice stays legal.
  Test: `tests/classpath_jvmstatic_object_property_e2e.rs`.

- **INSTANCE member of a classpath `object`, and dotted classpath nested types.** A plain (non-`@JvmStatic`)
  member call on a classpath `object` (`Ids.generate()`, `L.logger { }`) is an instance call on the
  singleton — `getstatic <Object>.INSTANCE; invokevirtual`. The qualified-name path previously errored it
  as an "unresolved Java static": it only tried `resolve_companion` (static) and the companion-object
  instance path, neither of which fits a bare `object`. The checker's Java-static fallthrough now, when the
  qualifier resolves to a classpath `object` (`LibraryType::is_object`), types the receiver as the object's
  own `Obj(internal)` and records `ObjectValue` so the existing instance-member + `INSTANCE`-read lowering
  fires. Separately, a dotted CLASSPATH nested type/qualifier (`Subject.User`, `SlugValidation.Ok`) resolves
  via a shared longest-prefix rule (outer simple-name → classpath internal, remaining segments joined with
  `$`, existence verified through `resolve_type`) — mirrored in both `resolve_ty` (checker) and `ty_ref`
  (lowerer) so `is`/`as`/`when` targets and a nested-class constructor (`Subject.User("x")` → `new
  lib/Subject$User`) all resolve the same `Outer$Nested` internal. Test: `tests/classpath_object_nested_e2e.rs`.
  Static access through `Outer.Nested.MEMBER` uses the same nested-name resolver in expression
  position, producing `<pkg>/Outer$Nested` before resolving the field.

- **A classpath MEMBER taking a RECEIVER lambda (`Recv.() -> R`) binds the lambda's `this`.**
  `@Metadata` marks such a value parameter with the `@ExtensionFunctionType` type annotation;
  krusty always decoded it (`MetaValueParam.recv_fun`/`recv_fun_receiver`) but previously wired it
  only into the call sig for TOP-LEVEL functions, so a member's
  `Builder.() -> Unit` parameter was indistinguishable from a leading value parameter
  `(Builder) -> Unit` and every lambda literal failed overload matching one arity short — a
  companion-object factory reached through the type name (`FactoryApi.create { … }`) fell
  through to the "unresolved Java static" catch-all. Member and receiver-less top-level metadata
  now share `CallSig::metadata_function`, which records the same
  `lambda_receivers`/`lambda_receiver_params`/`lambda_materialized` shape, and the checker's
  pre-selection lambda hook
  (`provider_member_lambda_expectations`, generalised from SAM-only) derives the expected shape of
  ANY function-typed parameter — receiver split out per the call-sig mark — typing the literal with
  `check_lambda_with_receiver_labeled`, mirroring the top-level HOF path; the hook's candidate
  probe also sees through `Type → companion object`, the same fallback the call resolution
  applies. EXTENSION call sigs stay unwired on purpose: extension calls already bind lambda
  receivers through `extension_lambda_shape`, and a second channel re-routed scope-function blocks
  (`run { this@C … }`) onto receiver-lambda paths whose lowering cannot resolve a labeled `this`.
  A generic receiver (`block: T.() -> R`) names no receiver class in metadata, so the expectation
  recovers it from the SUBSTITUTED parameter type. Expectations are mapped from source arguments
  through the selected `CallSig`'s semantic parameter slots before specialization, so reordered
  NAMED arguments and trailing/defaulted call shapes receive the declaration slot's lambda shape
  instead of whichever parameter happens to share the source argument's position.
  Test: `tests/classpath_companion_ext_lambda_e2e.rs`.

- **A SAFE call to a classpath member binds its lambda argument like the qualified call.**
  `re?.replace(s) { m -> m.value }` reaches the same `Regex.replace(CharSequence, (MatchResult) ->
  CharSequence)` as `re.replace(…)`, so the `?` must not change how the lambda's parameters bind.
  Two independent seams dropped that parity, and both are on the safe-call path only:
  - **Shape.** The safe-call argument seam (`Checker::ext_arg_tys`) had providers for SOURCE member
    shapes and EXTENSION shapes only; a classpath member's expectation had no provider there, so the
    lambda's parameters typed as `Any` and a member read on them reported "unresolved reference". It
    now falls back to the same `provider_member_lambda_expectations` the qualified path uses, against
    the NON-NULL receiver (`?.` narrows the receiver before member lookup). Provider order is source
    member > extension > semantic-provider member. That precedence is decided for the WHOLE call:
    the provider fallback runs only when neither a source member nor an extension supplied a shape,
    so a multi-lambda call can never combine parameter expectations from two competing callables.
    The qualified and safe-call paths therefore apply the same provider boundary.
  - **Selection.** The classpath member lookup in the safe-call arm passed argument TYPES only, so a
    lambda literal reached a Java functional-interface parameter as a plain `Ty::Fun` and matched no
    SAM parameter: the member did not resolve, the arguments were re-checked unshaped, and the shape
    above was discarded. It now resolves through the kind-aware entry point
    (`resolve_instance_member_with_literal_and_lambda_args`), the same VALUE-receiver channel the
    qualified arm uses, so SAM conversion and integer-literal adaptation apply after `?.` as before
    it. Keeping the complete receiver avoids a parallel bare-class-name selection path and consumes
    the resolver's canonical `ResolvedMember` directly.

  Together: a receiver function type (`Cfg.() -> String`) binds `this`, a plain function type binds
  its value parameters, and a Java SAM parameter binds its method's parameters, through `?.` as
  through `.`. Test: `tests/library_fun_type_lambda_param_e2e.rs`.

- **Function-typed CLASSPATH properties (`var handler: (Scope.(Req) -> Resp)? = null` in a
  dependency).** The JVM erases the property's shape everywhere the descriptor reaches: the field and
  accessor descriptors spell the raw `FunctionN` (all-`Any`), and even the accessor's generic
  `Signature` cannot spell a receiver mark (`Cfg.(A) -> B` and `(Cfg, A) -> B` share the `Function1`/
  `Function2` erasure). The `@Metadata` property type is the semantic authority; its decoded
  `generic_sig.ret` is projected through the same provider-boundary policy used for every structured
  member return. A concrete metadata FUNCTION type replaces a descriptor/`Signature` type only when
  both erase to the same JVM descriptor; this restores receiver/suspend facts that a JVM `Signature`
  cannot spell. Parameterized objects retain the signature-derived class identity, while incomplete
  collection metadata uses the existing same-family classifier overlay. This is deliberately not an
  accessor-name scan or a function-property exception: the `PropertySet` publishes one logical type to
  its property, getter and setter, while each opaque accessor keeps the physical descriptor used for
  emission; the ordinary member walk obtains a getter's declaration type through
  `metadata_property_ret_ty_name` and applies the identical guarded projection used for metadata
  functions and suspend returns. `concrete_generic_ret` likewise uses one complete-structured-shape
  rule for function and parameterized-object returns, including recursive JVM-to-Kotlin collection
  canonicalization (`List<Integer>` → `List<Int>`).

  A suspend function-typed property currently checks clean against kotlinc, but remains represented as
  its continuation-tailed metadata shape: `SUSPEND_TYPE` is consumed for aligned callable VALUE
  parameters, not yet as a blanket rewrite in the shared generic-type decoder. That distinction is
  intentional until every metadata carrier follows the same source-shape contract; applying the flag
  globally changes the established shape of coroutine-builder APIs such as `runBlocking` and makes
  their overloads disappear. Additionally, a lambda literal in a context whose EXPECTED type is a
  NULLABLE function type (`c.handler = { req -> … }` against `F?`) shapes against the non-null `F`, as
  kotlinc does — before, only a bare `Ty::Fun` expectation shaped the lambda, so the body's parameters
  read as `Any` and bare receiver calls were unresolved. Verified end-to-end against a kotlinc-compiled
  dependency (assignment, plain/suspend function types, receiver-style read/invoke, and a non-property
  function return containing collection types).
  Test: `tests/classpath_fun_typed_property_lambda_e2e.rs`.

- **Aliased imports (`import a.b.Member as Alias`).** The import map binds the alias directly to the
  full target for types and values. Ordinary lexical resolution handles local shadowing; lowering uses
  the resolved target member name.

- **Unqualified sibling nested-class construction (`Inner()` inside `class Outer { class Inner }`).** Kotlin
  scopes a nested class unqualified within its enclosing class body. When a `Name`-callee call is otherwise
  unresolved and the enclosing class (`this_ty`) has a nested class whose internal is `Outer$Inner`, the
  checker resolves it as constructing that class (a qualified `Outer.Inner()` already resolved). Exact-arity
  positional only; an `inner class` is excluded (it needs the enclosing instance — a synthetic `this$0` not
  in `ctor_params`), as are named/omitted-default nested ctors (later slices). The last-resort ordering
  keeps a real top-level `Inner` function/class winning. Test: `tests/nested_class_unqualified_e2e.rs`.

- **Unqualified sibling nested TYPE in a type position (`fun m(i: Inner)`, `val v: Inner`, return `Inner`).**
  Same nested-type scoping, for type references. Signature collection shadows `class_names` inside the
  `Decl::Class` arm with a clone extended by the class's own nested types' simple names (`Inner` →
  `Outer$Inner`, scanning hoisted `Decl::Class` named `Outer.<seg>`, one level deep), so member
  parameter/return/field types resolve; the checker's `resolve_ty` adds the same `this_ty`-scoped fallback
  for checker-only positions (local `val`, `as`/`is`). A nested type shadows an outer same-name type within
  the class body (Kotlin scoping); the fallback is last-resort so a real top-level/imported type still wins.
  The same nested fallback is mirrored in `resolve_ty_no_diag` (smart-cast narrowing) and the lowerer's
  `ty_ref`, so `is Inner` / `as Inner` on a nested type narrow/cast correctly. On a name COLLISION with a
  top-level type (`class Foo; class Outer { class Foo }`), ALL resolvers consistently pick the top-level
  (the signature-collection scope insert is skipped when the simple name already resolves), so the checker
  and codegen never disagree. Test: `tests/nested_type_scope_e2e.rs`.
- **A classifier nested in an INTERFACE scopes exactly like one nested in a class.** `interface C {
  class K; fun g(): K? }` is accepted by kotlinc: `K` is in scope for the interface's own member
  signatures (and `C.K` from outside). The interface body parser previously hoisted a nested
  classifier only when it was itself an interface, an annotation, or an implementor of the enclosing
  interface — a plain nested `class`/`enum class`/`object` was parsed and silently DROPPED, so both
  the member reference and the qualified outside reference read as unresolved (the exact shape of
  intellij's `plugins/textmate/core` `interface Constants { enum class StringKey … }`). Interface
  bodies now use the same `parse_and_register_nested_classifier` funnel as class/object bodies; the
  historical reason for the drop (a nested helper calling a PRIVATE interface member) is handled by
  the existing `access$` bridge synthesis and runs correctly
  (`interface_nested_classifier_e2e::interface_nested_class_calls_private_interface_member`). Byte
  parity with kotlinc holds for the minimal shape (`C` and `C$K`). Test:
  `tests/interface_nested_classifier_e2e.rs`.
- **A NESTED `value class` carries the full inline-class identity.** Three independent pieces, each
  wrong separately: (1) the shared nested-classifier funnel never set `is_value` — only TOP-LEVEL
  registration read the `value`/`inline` modifier — so `class C { @JvmInline value class V(val x:
  Int) }` registered as a PLAIN class and miscompiled (public `<init>`, identity `equals`, no
  `constructor-impl`/`box-impl`; a pre-existing hole for class owners that interface owners inherited
  when they stopped dropping nested classifiers); (2) the value-class mangle hashes the declared
  Kotlin FqName exactly as kotlinc spells it — dots throughout, so internal `I$V` hashes as `I.V`
  (`fun f(): V?` in `interface I` → `f--MlldnU`, not the `$`-spelled `f-IBQktzQ`); (3) a
  `JvmMethodSignature` in `@Metadata` records its name and desc INDEPENDENTLY, like kotlinc's
  serializer: the name only when a realization renamed the method (mangle/`@JvmName`), the desc only
  when the proto types don't pin the JVM descriptor — a mangled member whose value class BOXES
  (nullable `V?` return) is name-only; an ERASED shape (`h(): V` → `()I`) keeps the desc. Owner
  classes are byte-identical to kotlinc; the value-class BODY itself has a pre-existing member-ORDER
  divergence (top-level ones diverge identically), so its test asserts the ABI surface. Test:
  `tests/nested_value_class_e2e.rs`.
- **A hoisted anonymous object retains its construction site's lexical classifier scope.** The parser
  stores an anonymous object's class as a file-level synthetic declaration, but its member signatures,
  supertype arguments, superclass constructor arguments, and inferred member returns may still name a
  class nested in the source owner (`object : Base(Inner()) {}` inside `Outer`). A structural map from
  anonymous declaration to containing class is computed by the same generic expression-target walk used
  for capture containment, and one cycle-safe declaration-chain primitive feeds signature collection,
  return pre-inference, and the main checker. The chain contributes classifier scope only: it never adds a
  runtime receiver, changes capture fields, or alters the anonymous class ABI. Generated anonymous JVM
  names are exact roots; `$` characters in them are not parsed as evidence of source nesting. Tests:
  `tests/nested_class_ctor_scope_e2e.rs` and
  `resolve::tests::anonymous_object_records_its_lexical_source_class_owner`.
- **Named arguments to a CLASSPATH constructor (`Point(y = 2, x = 1)`).** Descriptors don't carry
  parameter names, so this needs the ctor's `@Metadata`: `metadata::class_constructor_param_names` decodes
  `Class.constructor` (field 8) → `Constructor.value_parameter` (field 2, a DIFFERENT proto shape from a
  `Function` — no name/return, value-parameters at field 2 not 6) → `ValueParameter.name`. Exposed via the
  `SymbolSource::constructor_param_names` hook; the checker's named-argument gate and the lowerer's
  classpath-`new` both reorder the labelled arguments onto positions (via `reorder_by_param_names`) before
  resolving/emitting. Test: `named_args_classpath_e2e` / `interface_supertype_members_e2e`.
- **Named args / omitted defaults on a QUALIFIED nested-class constructor (`Op.Ext(a = 1, b = "x")`,
  `Op.Ext(a = 1)`, `Op.Ext(4)`).** A qualified nested ctor's receiver names a TYPE, not a value, so the
  named-argument gate resolves it through the committed classifier-segment walk WITHOUT typing the receiver as a value
  (which errored "unresolved reference"). The nested-ctor construction path then maps labels onto positions
  (`constructor_named_params` + `map_call_args`, with `synthetic_default_ctor` for an omitted defaulted
  param) and resolves positional forms via `library_ctor_resolves` (covering the `<init>$default`
  synthetic); the lowerer routes a named call to `lower_external_new_named`, positional to
  `lower_external_new`. Test: `tests/classpath_qualified_nested_named_ctor_e2e.rs`.
- **Classpath `typealias` (`import lib.Alias` for `typealias Alias = Real`).** A top-level type alias lands
  in its FILE FACADE's `@Metadata` (`LibKt`), not only the stdlib's dedicated `*TypeAliasesKt` files, so the
  classpath type scan parses `Package.typeAlias` (proto field 5 → name field 2 + EXPANDED type field 6,
  falling back to the underlying type field 4) from EVERY `*Kt` facade (`metadata::package_type_aliases`).
  This proto reader replaced a `d2` `$annotations` heuristic that a facade's annotated top-level property
  would have tripped. Resolves the alias as a constructor and in a type position. Test:
  `tests/classpath_typealias_e2e.rs`.
- **A classpath declaration belongs to the package its `@Metadata` NAMES, not the directory its class
  file sits in.** `@JvmPackageName` moves an emitted file facade out of its declared Kotlin package
  and records the declared one in `@Metadata`'s `pn` element (a `s`-tagged String, absent on every
  unrelocated class). kotlin-test's JUnit5 variant is the shape every Kotlin test source hits:
  `package kotlin.test` with `@file:JvmPackageName("kotlin.test.junit5.annotations")`, so
  `typealias Test = org.junit.jupiter.api.Test` is declared in `kotlin.test` but emitted to
  `kotlin/test/junit5/annotations/AnnotationsKt`. krusty keyed every classpath alias by the JVM parent
  of its facade, filing `Test` under `kotlin/test/junit5/annotations/Test`, so `import kotlin.test.Test`
  reported `unresolved reference 'Test'` on every `@Test`-annotated function. `pn` is now decoded once
  in `classreader` and is the single declaring-package fact (`KotlinMeta::package`) that keys the alias
  table, the class-directory facade recovery, and the per-package facade admission — no channel infers
  a declaring package from a class's location. Test:
  `tests/classpath_relocated_facade_typealias_e2e.rs`.
- **A classpath TOP-LEVEL property is a value (`import kotlin.math.E; import pkg.plugin`).** A package's
  namespace record carried its top-level FUNCTIONS and its EXTENSION properties but never its receiver-less
  top-level properties, so every use site — explicit import, star import, same package — reported
  "unresolved reference". The facade property scan now classifies by the accessors' receiver parameter
  (`PropKind::TopLevel` when the getter takes none, `Extension` when it takes one), the resolver's
  the generic symbol query selects it over the import scope (ambiguity across two in-scope packages is
  no resolution), and a read lowers to the declaring facade's static getter (`ExprLowering::
  TopLevelPropertyGet`). It is the LAST value rung: every enclosing scope shadows an imported property.
  READS only so far: a `const val` top-level (no getter — its value inlines from a static field) and a
  WRITE to a top-level `var` (the setter is decoded and carried, but assignment does not reach this rung)
  are both still reported unresolved. Test: `tests/classpath_top_level_property_e2e.rs`.
- **A RECEIVER function type survives the classpath decode (`configure: Cfg.() -> Unit`).** `Cfg.() -> Unit`
  and `(Cfg) -> Unit` share one `Function1` erasure, so the distinction lives ONLY in `@Metadata`'s
  `@kotlin.ExtensionFunctionType` type annotation. Two decoders dropped it: the metadata signature reader
  (`parse_type_gsig_node` built `Ty::Fun` from the `kotlin/FunctionN` classifier alone) and every MEMBER,
  whose signature comes from the JVM `Signature` attribute — which cannot spell it — and whose metadata call
  facts omitted the per-parameter marks. Both now carry it: the metadata reader honors the annotation, and a
  member's decoded signature is re-marked from metadata (`mark_receiver_fun_params`) and reused rather than
  re-parsed. A lambda argument to such a parameter is shaped from the parameter itself
  (one `LambdaCallShape`, the same vocabulary the module and extension shape providers speak, so a call
  site types its lambda from ONE shape whatever the callable's origin) — for members and top-level alike,
  and the
  receiver comes from the generic signature (with its type ARGUMENTS bound by the call) in preference to
  metadata's receiver CLASS. A `suspend` callable's physical signature appends a `Continuation` its source
  parameter list does not have, so both alignments (the marks, and lambda specialization) drop it first.
  A classpath CONSTRUCTOR with a receiver-lambda parameter (`Builder { … }`) is still not shaped — the
  constructor query takes plain argument types and never sees the lambda literal. Tests:
  `tests/classpath_member_receiver_lambda_e2e.rs`, `tests/classpath_receiver_lambda_overload_e2e.rs`.
- **Omitting a defaulted argument does not change what an argument may be.** A classpath call that omits a
  trailing default measured applicability with the platform-only "same erased shape" check, so any SUBTYPE
  argument was rejected — `host(sub)` reported unresolved while `host(sub, 5)` resolved. The defaulted path
  now asks the same assignability question the spelled-out path asks — and then RANKS: applicability admits
  both `pick(b: Base, n: Int = 3)` and `pick(s: Sub, n: Int = 4)` for `pick(Sub())`, so the most specific
  parameter shape is tried first (declaration order breaks ties). Test:
  `tests/classpath_default_arg_subtype_e2e.rs`.
- **An omitted default is recorded the same way however the receiver is spelled.** A classpath EXTENSION
  call omitting a defaulted argument resolves to the `$default` synthetic, whose emit needs the call's
  argument→parameter mapping. Only the explicit-receiver spelling recorded one, so the same call on an
  IMPLICIT receiver (`build { tag("a") }`) skipped the whole file with "not yet supported by the IR
  backend". The record exists to carry a mapping the call's own shape does not give (labels, reordering);
  unlabelled, the shape gives it — positional arguments fill parameters left to right and a TRAILING
  LAMBDA binds the LAST parameter, so an omitted default may sit BETWEEN them — and the emit derives it
  instead of treating its absence as "unknown". Derived at the emit rather than recorded by the checker so the
  paths that never reach it — an `inline` extension is SPLICED, never emitted as a `$default` call — keep
  behaving as they did. A vararg call is excluded: its trailing slot is an array the emit builds, not an
  omitted parameter, and so is a callable past 32 parameters, whose `$default` ABI takes several mask
  ints the emit does not yet build. Test: `tests/classpath_extension_default_implicit_receiver_e2e.rs`.
- **A failed constructor probe leaves the call's arguments as it found them.** For `Name(args)` where
  `Name` is both a classpath class and a top-level function, the constructor is probed first; it re-checked
  every argument with no expected type, overwriting a trailing lambda already shaped against the function's
  receiver parameter with a bare `() -> Unit` — after which neither candidate accepted the call. The probe
  now types only arguments the call has not typed yet. Test:
  `tests/classpath_ctor_vs_same_named_function_e2e.rs`.
- **A `suspend` member's return type is recovered from its `Continuation<T>` generic argument.** The
  generic argument carries a PRIMITIVE return BOXED (generics erase primitives to wrappers), so a non-null
  primitive return unboxes to its Kotlin primitive (`java/lang/Long` → `Ty::Long` via
  `jvm_class_map::wrapper_to_kotlin_prim`), and a reference is canonicalized (`java/lang/String` →
  `kotlin/String`). Nullability applies (`ret_nullable`) only to a PRIMITIVE return — a nullable primitive
  is a distinct boxed type — while a nullable REFERENCE keeps its plain erased `Ty`, exactly as `resolve_ty`
  treats a declared `String?` (reference nullability is not carried in `Ty`), so the recovered suspend
  return matches a source-spelled reference return instead of a divergent `Ty::Nullable`. Test:
  `tests/suspend_return_type_recovery_e2e.rs`.
- **A generic-return builtin member's nullability is recovered from `.kotlin_builtins` metadata**
  (`kotlin/collections/Map.get(K): V?`, `getOrDefault`, …). When the mapped JVM class IS on the classpath,
  the member that resolves the call is the erased classpath method (`java/util/Map.get` → `Object`), which
  carries no Kotlin nullability. The source `V?`
  survives only on the builtin's `Type.nullable` flag; `parse_builtins` records every function member's
  return-nullability (including the dropped ones) in `BuiltinClass.member_ret_nullable`, and the member
  walk (`Classpath::builtin_member_ret_nullable`) null-annotates the resolved return. Applied only to a
  PRIMITIVE return — a nullable primitive is a distinct boxed type, so `m[k] ?: d` must null-check before
  unboxing (else a null `Integer` unboxes → NPE); a nullable REFERENCE already null-checks regardless and
  keeps its plain erased `Ty` (mirrors the suspend/`resolve_ty` policy above). This is why `m[k] ?: continue`
  correctly skips absent keys. NOT a hardcoded method list — the flag is read from `@Metadata`. Test:
  `tests/map_get_nullable_elvis_e2e.rs`.
- **`.kotlin_builtins` types decode in full — type parameters AND type arguments.** A builtins `Type` is
  either a `class_name` with `argument`s, or a reference to a declared `type_parameter` (by id, or by
  `type_parameter_name`); the decoder resolves all three, and each `Class`/`Function`/`Property` carries
  its own `type_parameter` table naming those ids. Members are therefore never dropped for having a
  type-parameter type (`List<E>.get(index: Int): E`, `MutableList.removeAt(Int): E`), and a type argument
  survives (`Map<K, V>.entries: Set<Map.Entry<K, V>>`). Since a builtin member has no JVM `Signature`
  string, `builtin_members` also carries a DECODED `LibraryMember::generic_sig` (erased `params`/`ret`
  matching the descriptor, declared ones in the signature), and `Classpath::builtin_class_gsig_name`
  supplies the builtin's formals + argument-carrying supertypes where a class `Signature` normally would.
  Together these let the member walk bind a type-parameter return against the receiver's type arguments
  (`List<String>.get(1): String`) with NO JDK on the classpath — the `.kotlin_builtins` fallback
  configuration, where the mapped JVM class (`java/util/List`) is absent. Tests:
  `tests/metadata_return_types.rs` (`builtins_decode_type_parameters_and_arguments`,
  `builtin_generic_member_binds_receiver_argument_without_jdk`,
  `builtin_generic_members_type_check_without_jdk`).
- **A JDK-less compile EMITS the same bytecode a JDK-present one does.** Every realization fact the
  backend normally reads off the mapped JVM class file — interface-ness, the physical accessor name,
  the erased descriptor — is also carried by the builtin's own `.kotlin_builtins` entry, so the absence
  of `java/util/List.class` changes what the compiler READS, never what it emits. Three facts have to
  survive that route, and each was independently lost before:
  - **Interface dispatch.** `Classpath::builtin_members_name` takes interface-ness from the builtin's
    `CLASS_KIND`, but a `LibraryMember` round-trips through `FunctionInfo`/`LibraryCallable` during
    overload selection, which dropped the bit — so the call site fell back to
    `library_type_is_interface(owner)`, which cannot answer for an absent `java/util/List`.
    `LibraryCallable::owner_is_interface` now carries it and `FunctionInfo::member_with_return`
    restores it, the same way `suspend` travels with the selected overload.
  - **The physical accessor name.** A property read asks `MethodBodies::property_read_access` for the
    owner's declared accessor; with no class file that returned `None` and the backend invented the
    JavaBean getter (`getSize`, `getEntries`). `Classpath::property_read_access` now falls back to
    `builtin_property_read_access`, which walks the builtins supertype closure and answers with the
    mapped `java.util` spelling (`size`, `keySet`, `entrySet`) from the same
    `builtin_property_jvm_name` mapping the member table uses — one definition, so a call and a
    property read of the same builtin cannot disagree.
  - **Return erasure.** That fallback also supplies the member's OWN (already erased) descriptor, so a
    type-parameter-typed property emits `getKey:()Ljava/lang/Object;` + `checkcast`, not a descriptor
    rebuilt from the substituted use-site type (`getKey:()Ljava/lang/String;`, which no class declares).
  Interface-ness for an owner with no class file likewise comes from the builtin `CLASS_KIND`
  (`Classpath::owner_is_interface`), replacing a curated JVM-name table that omitted every `java/util/*`
  and so answered "class" for all of them. A fourth fact travels the same route:
  - **The nesting relation.** A reference to a NESTED builtin (`java/util/Map$Entry`) makes the class
    carry an `InnerClasses` entry, which `backend::classpath_inner_class_resolver` read off the
    enclosing class file; with no JDK the attribute vanished entirely. A `$`-separated JVM name
    decomposes structurally, its enclosing half maps back to a Kotlin builtin, and the
    `.kotlin_builtins` fragment declares the nested class (`kotlin/collections/Map.Entry`) with the
    `Class.flags` word that yields the JVM access flags the entry records
    (`Classpath::builtin_nested_class` over `metadata::builtin_class_access`). Requiring that
    declaration to exist is what keeps a `$` that is merely part of a mangled name from being reported
    as nesting. VISIBILITY/MODALITY/CLASS_KIND/IS_INNER map onto ACC flags the same way kotlinc's own
    class emit does, so the recovered entry equals the one javac put in `java/util/Map` byte for byte.
    Two arms are worth naming: `internal` is `ACC_PUBLIC` (kotlinc mangles the NAME, it does not narrow
    the flag), and a `Class` message may omit `flags` entirely (`kotlin/String`, `kotlin/Int`, every
    `kotlin/*Array`). The parser applies the protobuf default `6` (`public final`) at its wire boundary;
    omission therefore never masquerades as the explicit zero word for `internal` in later phases.
  Tests: `tests/no_jdk_builtin_emit_e2e.rs` (each defect as a `box()` that is actually LOADED and RUN on
  a JVM, plus a byte-for-byte JDK-less vs JDK-present emit comparison — a diagnostics-only assertion
  cannot see any of this, which is how all of them shipped green) and
  `metadata::builtin_class_access_tests` for the flag-word mapping.
- **`MutableList.removeAt(Int)` IS `java.util.List.remove(int)`** — the function half of kotlinc's
  `BuiltinMethodsWithDifferentJvmName`/special-builtin renaming whose property half is
  `size`/`keys`/`values`/`entries`. A call through a `MutableList` receiver emits the JVM name
  (`names::mapped_builtin_virtual_name`), and a class implementing `MutableList` gets a `remove(int)`
  bridge forwarding to its `removeAt` override (`mapped_interface_members` →
  `bridges::mapped_interface_bridges`) — needed when the override is inherited from a NON-collection
  supertype, which is the only place the two names can diverge. Unlike the `size` entry beside it, this
  one is keyed on the KOTLIN name `kotlin/collections/MutableList`, not the erased `java/util/List`:
  the renaming exists only on the mutable side, so a READ-ONLY `List` implementation that happens to
  declare an unrelated `removeAt` must not acquire a `remove(int)` bridge. Tests: box corpus
  `codegen/box/specialBuiltins/irrelevantRemoveAtOverride.kt`, and
  `tests/metadata_return_types.rs::read_only_list_impl_gets_no_remove_bridge`.
- **A classpath method/interface member with a Kotlin-COLLECTION parameter (`fun size(items: List<String>):
  Int`) resolves.** The JVM method descriptor erases a collection parameter to its single JVM interface
  with the type argument dropped (`List<String>` → `Ljava/util/List;`), but the call passes the Kotlin type
  itself (`h.size(listOf("a"))` → arg `kotlin/collections/List<String>`). The exact / `Any`-widened /
  subtype overload passes in `select_instance_info` all compared `java/util/List` against
  `kotlin/collections/List<String>` and missed → `unresolved method 'size' on 'lib/H'`. A final pass now
  matches BOTH parameter and argument in their JVM-descriptor form (`SymbolSource::jvm_descriptor_form`),
  bridging the collection identity and erasing type arguments — the METHOD analog of the constructor path
  `resolve_constructor` already had. Runs LAST (after the specific passes) and only when an argument's form
  actually changes (`jvm_args != args`), so it never alters existing overload selection, keeps distinct
  interfaces distinct (`java/util/List` ≠ `java/util/Set`), and never coerces a scalar. This single root
  covered two reported failures: a plain method with a `List<T>` param, and a `suspend` interface member
  whose `get(ids: List<Int>): List<Info>` PARAM (not its return) was the actual unresolved-member cause.
  Test: `tests/classpath_collection_param_member_e2e.rs`.
- **`kotlinx.coroutines.runBlocking { … }` (a classpath coroutine builder) resolves, lowers, and RUNS.** Two
  coordinated pieces. RESOLUTION: `runBlocking { }` passes ONE trailing lambda against TWO parameters (a
  defaulted `context` + the `block`). `default_omit_lambda_param_indices` aligns the trailing lambda to the
  LAST parameter (omitting leading defaults) so the checker's lambda helpers type the block and the call
  resolves to `BuildersKt.runBlocking$default`. The alignment is gated behind `has_exact` — it applies ONLY
  when NO overload of that name matches the argument count exactly, so a plain `run { … }` (which HAS an
  exact-arity overload) never mis-binds against a wider same-named overload. LOWERING: the block is `suspend
  CoroutineScope.() -> T`, erased in the descriptor to a bare `Function2` with no `suspend` flag; `lower_arg`
  detects the suspend lambda STRUCTURALLY (its checked `Ty::Fun` ends in a `Continuation` param) and routes
  it to `lower_suspend_lambda`, which builds the real `SuspendLambda` state machine (the `CoroutineScope`
  receiver binds as the body's implicit `this`, like any receiver lambda). The lambda body is lowered as a `suspend` context
  (`cur_fn_suspend`) so a suspend MEMBER call inside it (`repo.get(…)` on a classpath `suspend` interface) is
  CPS-threaded, and `suspend_member_call` detection consults the library for classpath members. Supports a
  non-suspending body, a tail suspend call, and a bound suspension (`val x = work(); …`); a suspension nested
  in an `if`/`when` CONDITION cleanly SKIPS (the pre-existing flattener limit), never miscompiles. Test:
  `tests/classpath_runblocking_e2e.rs`.
- **An under-applied VALUE-CLASS-parametered builder with a trailing lambda resolves, lowers, and RUNS**
  — the value-class-parametered sibling of the `runBlocking` case, and the shape
  `kotlinx.coroutines.test.runTest { … }` has. A builder
  `run…(timeout: kotlin.time.Duration = …, testBody)` mangles its
  JVM name (`sourceName-<hash>`) AND its `$default` synthetic because of a value-class
  parameter, which broke the call at TWO seams. METADATA ALIGNMENT (`classpath.rs`): `@Metadata` names
  the value class while the descriptor carries its erased underlying (`J`), so `meta_param_compat` /
  `meta_param_exact` now resolve the underlying through the platform's value-class knowledge
  (`value_underlying_name`, threaded into `aligned_meta_index` / `metadata_call_facts_name` /
  `aligned_generic_sig_name` / `is_inline_callable_name` for top-level/static callables and into
  `aligned_member_metadata` / `metadata_member_shape_matches` / `metadata_member_descriptor` for
  members; unsigned underlyings normalize like the mapped builtins, `UInt` → `Int`) — before, alignment failed and the function
  silently lost its parameter names/defaults, making every under-applied call inapplicable. DEFAULT-CALL
  LOOKUP (`symbol_resolver.rs`): `resolve_top_level_default_callable` probed only the SOURCE spelling
  (the unmangled overload); it now also resolves each mangled spelling's
  `$default` directly in its base candidate's facade package (the import scope only knows the source
  name). Tests: `tests/classpath_value_class_builder_e2e.rs` — a kotlinc-built FIXTURE reproducing the
  shape (mangled name + mangled `$default` + `@JvmMultifileClass` part), so the coverage owns its
  dependency instead of pinning a third-party jar version — and `jvm::classpath`
  `metadata_param_matching_*`.
- **An imported Java STATIC accepts a lambda for a SAM-interface parameter** (`import
  org.junit.jupiter.api.Assertions.assertThrows`; `assertThrows(T::class.java) { … }`, `import
  java.util.concurrent.CompletableFuture.runAsync`). Two gaps made the unqualified call unresolved.
  RESOLUTION (`resolve.rs`): the imported-static path disambiguated overloads with TYPED argument
  kinds, collapsing the lambda to a plain `FunctionN` that never matches a Java SAM parameter; it now
  routes through `resolve_companion_with_literal_args` so the lambda stays a `LambdaLiteral` and the
  `classpath_sam_arg_matches` rule in `best_companion_overload` applies. CHECKING: the selected
  member's arguments were re-checked by raw assignability (`() -> Unit` vs `Runnable` → mismatch); a
  lambda argument against a classpath SAM parameter is now checked against the SAM method's parameter
  types (`check_lambda_with_types`), mirroring the qualified-call path. Test:
  `tests/static_member_import_e2e.rs`.
- **A generic classpath `suspend` member returning a TYPE PARAMETER binds it from the receiver's type
  argument** (`interface Repo<T> { suspend fun byId(): T? }` on a `Repo<Cfg>` receiver → `Cfg?`). The
  non-suspend member path binds `T` via `member_return` (substituting the receiver's args into the generic
  return), but the suspend path recovers its return from the `Continuation<T>` generic signature and had NO
  substitution — so `T` erased to `Any`, and `r.byId() ?: error(…)` then `c.at` failed with "member … on
  'kotlin/Any'". `receiver_type_bindings` computes the receiver→declaring-class formal→argument map (the same
  hierarchy walk `member_return` performs) and `suspend_return_from_gsig` substitutes the recovered bare type
  parameter under it. Test: `tests/generic_suspend_member_return_e2e.rs`.
- **A classpath constructor accepts a NOMINAL-SUBTYPE argument** (`Outer(s: Sub)` called with a sealed/open
  subclass `Sub.U(…)`). The `<init>` overload resolution matched an exact / value-class-erased /
  JVM-collection-erased argument, and its subtype pass was gated behind `jvm_args != args` (only when a
  collection/value-class argument changed form) — so a plain reference subtype (no erasure) skipped it and
  `Outer` was reported unresolved. `resolve_constructor` now has a general nominal-subtype fallback (walk each
  argument's classpath supertype closure to its parameter, via `ctor_arg_subtype_of_param`) AFTER every exact
  pass, so the most-specific constructor still wins and a scalar parameter is never coerced (the widening is
  restricted to reference `Ty::Obj` arg↔param pairs). Test: `tests/classpath_subtype_ctor_arg_e2e.rs`.
- **An `is`-check smart-cast narrows to a CLASSPATH subtype** (`val v: V; if (v is V.Ok) v.v`, where `V`/
  `V.Ok` are classpath types). The speculative narrowing type resolver `resolve_ty_no_diag` resolved only
  same-module (user) classes, type parameters, and a sibling nested type of the enclosing class — a classpath
  / imported type erased to `Ty::Error`, so the narrowing was dropped and `v` kept its parent type ("member
  … on `<parent>`"). It now uses the same committed classifier-segment walk as `resolve_ty`, so both a
  positive `is` and a negated `!is`/else narrowing work. (`as` casts already resolved classpath types.) Test:
  `tests/classpath_is_smartcast_e2e.rs`.
- **A classpath EXTENSION whose value parameter is a VALUE CLASS resolves** (`inline fun <reified T>
  Reg.getFor(id: Id): T`, `Id` `@JvmInline`). The value-class parameter `@JvmName`-mangles the extension's
  bytecode name (`getFor-<hash>`) and erases the parameter to its underlying, so the literal-name extension
  index missed it and the argument (`Id`) failed to match the erased-underlying (`String`) parameter →
  "unresolved method". A new extension-query handler maps the source name → the mangled `jvm_name` via
  `@Metadata` (extension receiver == the receiver, at least one value-class value parameter) and exposes it
  with LOGICAL value-class parameter types; `bound_logical_params` prefers a value-class logical parameter
  over the erased-underlying `Signature` so the value-class argument matches. An inline extension is marked
  `must_inline` — an inline function MUST be spliced (or the call SKIPS); krusty never falls back to an
  `invokestatic` of an inline body (that is never correct, and a reified extension's bytecode is only a
  throwing stub). A reified inline extension whose body krusty cannot yet splice from bytecode therefore
  skips at lowering rather than miscompiling. Test: `tests/classpath_valueclass_param_ext_e2e.rs`.
- **A TOP-LEVEL `suspend fun` applying an inline collection HOF to a suspend call's result** (`suspend fun
  f() = source().filter { it > 0 }`) emits (the class-method form already worked). The CPS transform appends
  a `Continuation` parameter and shifts every body value-index `>= threshold` up by one (`shift_locals`);
  the threshold is 0 for a top-level function (no `this`). The old shift descended into the NESTED lambda
  body, bumping the `filter`/`map` predicate's own `it` (value-index 0 → 1) — the lambda is extracted to a
  method whose parameter stays at index 0, so its now-`GetValue(1)` read referenced an unallocated slot (a
  class method escaped because its lambda `it`=0 was below the threshold 1). `shift_locals` now delegates to
  `ir::shift_value_indices`, which shifts a lambda's CAPTURES (enclosing-frame reads) but NOT its body (a
  separate value-index scope) — so a capturing predicate (`filter { it > k }`, `k` a body local) still works.
  Test: `tests/build688_ff1_suspend_hof_e2e.rs`.
- **A classpath `suspend` method with a defaulted parameter, called with that argument OMITTED**
  (`class S(r) { suspend fun list(f: Filt = Filt()): Int }`, called `s.list()`). A suspend method's
  `$default` synthetic carries the `Continuation` as a real trailing parameter of the original method —
  `list$default(S, Filt, Continuation, int mask, Object marker)`, the `Continuation` BEFORE the mask/marker.
  The default-member lowering matched only the non-suspend shape (`… int, Object`) and the coroutine pass
  APPENDED the continuation after the marker, so the `int` mask landed in the `Continuation` slot
  (VerifyError). `synthetic_default_member` now also recognises the suspend shape (Continuation before
  mask/marker) and `append_continuation` INSERTS the continuation value at that position for a `$default`
  call rather than appending it. Test: `tests/suspend_default_param_e2e.rs`.
- **A `suspend` body accessing a member of a suspend call's result inline (`suspend fun f(r) =
  r.all().size`).** The CPS flattener only meets a suspension at a bound-local / bare-statement position;
  a suspension nested in a `return`/member-access value must be pre-hoisted. `hoist_suspensions` now
  descends into a NON-suspend `Call` (dispatch-receiver + args), `MethodCall` (receiver + args) and
  `GetField` (receiver) — all of which evaluate their children unconditionally before the access — hoisting
  each suspension to a preceding `val tmp = <call>` temp the flattener handles (`return r.all().size` →
  `val tmp = r.all(); return tmp.size`). Conditional nodes (`if`/`when`/elvis) and lambda bodies are left in
  place. Test: `tests/suspend_member_after_call_e2e.rs`.
- **A `suspend` body applying a kotlin.collections INLINE HOF / extension to a suspend call's collection
  result (`val m = r.cfg(); m.map { … }`, `r.cfg().first()`, `m[0]`).** Two fixes. (1) The suspend return
  was recovered in erased JVM form (`Continuation<List<T>>` spells the collection in Java terms —
  `java/util/List`), on which the kotlin.collections extensions aren't keyed. `suspend_return_from_gsig`
  now canonicalizes a JVM collection to its Kotlin type (`jvm_class_map::jvm_collection_to_kotlin`), and
  the member walk recovers the EXACT read-only-vs-mutable form (`List` vs `MutableList`) from the member's
  `@Metadata` return type (the aligned call facts/property fallback + guarded overlay below) — which
  the JVM signature erases — so a declared `MutableList` return keeps `.add(…)`. (2) The CPS `box_returns` pass hit its
  `_ => false` fallthrough on a LAMBDA argument in `return m.map { … }`, bailing the state machine; a lambda
  argument is a value (its body is a separate impl function, not a `return` of the suspend fn) so it is now
  a leaf there (varargs recurse into their elements). Test: `tests/suspend_collection_hof_e2e.rs`.
- **A top-level property's backing field carries its generic `Signature`.** A top-level `val xs:
  List<String>` becomes a static field of the FILE FACADE, whose field table is built by
  `emit_statics` rather than the class-field path — so it dropped the `Signature` the same property
  declared inside a class already carried, and a consumer read `java.util.List` where kotlinc
  records `Ljava/util/List<Ljava/lang/String;>;`. The rule is the class path's: a type with type
  arguments carries its full generic signature, a type without carries none. Its ACCESSORS carry the
  same signature (`getXs()` → `()Ljava/util/List<Ljava/lang/String;>;`, `setXs` →
  `(Ljava/util/List<Ljava/lang/String;>;)V`), interned between the accessor's descriptor and its
  nullability annotation — kotlinc reaches it before the body's field cluster, so seeding it later
  would shift every following pool entry. Tests:
  `tests/generic_signature_e2e.rs::top_level_property_field_gets_its_generic_signature` and
  `::top_level_property_accessors_get_their_generic_signatures`.
- **A `private` classifier is package-private in the class file, for every declaration kind.** The JVM
  has no class-level `private`, so kotlinc drops `ACC_PUBLIC` and keeps the real visibility in
  `@Metadata` (and in `InnerClasses` for a nested classifier); `internal` stays `ACC_PUBLIC`, since the
  module boundary is a Kotlin-only fact. This holds for ordinary, data, sealed, value, object,
  interface, fun-interface, enum, and annotation forms alike. Their syntax-specific parser arms now
  publish through one classifier boundary that records visibility; previously only the plain `class`
  arm did. A local classifier is a measured control: it has no declared visibility and kotlinc keeps
  its own class flags `ACC_PUBLIC`. Test: `tests/private_classifier_access_e2e.rs`.
- **A `private` primary constructor is `ACC_PRIVATE` unless another class constructs the type.**
  kotlinc emits the constructor private and, WHEN a site outside the class constructs it (a companion
  factory is the common shape), adds a synthetic `public` `(…, DefaultConstructorMarker)` bridge that
  delegates to it; the cross-class `new` then passes `aconst_null` for the marker. krusty does not emit
  that bridge yet, so it chooses the flag by the construction sites it can see: `ACC_PRIVATE` when
  nothing outside constructs the class (kotlinc's shape, and the common case), `ACC_PUBLIC` when
  something does — `ACC_PRIVATE` without the bridge would make the cross-class construction an
  `IllegalAccessError`. "Outside" is decided by INVERSION: the constructions reachable from the
  class's own declarations are collected, and any other construction of it in the file counts as
  external. A DEFAULT ARGUMENT is the case that makes this necessary — it is evaluated at the call
  site, so a nested class whose parameter defaults to `Hidden(…)` constructs the private constructor
  from a different JVM class, and enumerating only function bodies missed it. `@Metadata` records the declared privacy either way. A SECONDARY constructor's
  visibility is not modeled at all yet (`IrSecondaryCtor` has no visibility). Test:
  `tests/private_constructor_access_e2e.rs`.
- **A member's `$default` synthetic opens with kotlinc's super-call guard, when its owner can be
  inherited from.** `super.m()` carrying defaults cannot be dispatched — the stub would re-enter the
  OVERRIDE through `invokevirtual` — so kotlinc passes a NON-NULL trailing marker at such a call site
  and the stub throws: `aload <marker>; ifnull L; new UnsupportedOperationException; dup; ldc "Super
  calls with default arguments not supported in this target, function: <name>"; invokespecial; athrow;
  L:` with a `same_frame` at `L`. Which owners get it was MEASURED against kotlinc: an `open`,
  `abstract` or `sealed` class, and an `enum class` (whose entries may carry bodies and so subclass
  it); NOT a final class — including a `data class`, a nested class, a companion object or a private
  one — nor an interface's `$DefaultImpls`, nor a file facade, none of which can receive such a
  `super` call. Test: `tests/default_stub_super_guard_e2e.rs` (differential over every owner shape,
  plus a run proving an ordinary defaulted call still works with the marker null).
- **Declaration-site variance becomes a JVM wildcard in a PARAMETER position only.** Kotlin's
  declaration-site `out`/`in` has no classfile equivalent, so the backend realizes it as a wildcard on
  each otherwise-unprojected argument — but kotlinc does that for method PARAMETERS only. A return
  type, a field type and a getter's return spell every argument invariantly, at EVERY nesting depth:
  `fun <U> deep(a: Map<String, List<U>>): Map<String, List<U>>` signs its parameter
  `Ljava/util/Map<Ljava/lang/String;+Ljava/util/List<+TU;>;>;` and its return
  `Ljava/util/Map<Ljava/lang/String;Ljava/util/List<TU;>;>;`. krusty wildcarded both, so every generic
  return and field diverged from the reference bytes. An EXPLICIT `in`/`out` projection is the user's
  own and renders in either position (`Comparator<in Number>` keeps its `-` in a return); an explicit
  `out` on an already-`out` parameter is redundant and Kotlin normalizes it away before the backend
  sees it. A CONSTRUCTOR parameter is a parameter position even when the same declaration also backs a
  field: `class Box(val c: Container<Number>)` with `class Container<out T>` signs its `<init>`
  `(LContainer<+Ljava/lang/Number;>;)V`, its field `LContainer<Ljava/lang/Number;>;` and its getter
  `()LContainer<Ljava/lang/Number;>;`. A suspend function's return travels as a `Continuation<-RET>`
  PARAMETER and wildcards inside it. Realized as a `Wildcards` mode threaded through the signature formatter. Test:
  `tests/generic_signature_e2e.rs::declaration_site_wildcards_appear_in_parameter_positions_only`.

- **A classpath member's (function OR property) declared collection mutability survives at EVERY nesting
  level.** The JVM `Signature` attribute erases read-only vs mutable (`List`/`MutableList` both spell
  `java/util/List`) at every depth, so signature-derived resolution canonicalized `fun items():
  MutableList<String>` / `val bag: MutableList<String>` / `fun nested(): MutableList<MutableSet<String>>`
  to their read-only forms and `.add(…)` was a false "unresolved reference". The `@Metadata` return type
  preserves the exact classifiers: the already-aligned `MetadataCallFacts::declared_ret` carries a
  metadata FUNCTION's full return without a second overload lookup;
  `Classpath::metadata_property_ret_ty_name` handles a property GETTER (matched by its
  `JvmPropertySignature` — a getter is NOT a metadata function, and class-member properties are
  `metadata::class_properties`, not the package-level `meta_properties_name`); and
  `overlay_metadata_collection_names` overlays the classifiers onto the
  signature-derived type level by level. Guard per level: a metadata name replaces the signature's ONLY
  when the shared erasure table identifies a Kotlin collection sibling mapping to the same JVM internal
  (`is_kotlin_collection_type_name` + `type_names_map_to_same_jvm_internal`), and the walk descends into
  type arguments only when the classifiers agree with matching arity — a divergent classifier (stale
  metadata) never forms an arity-mismatched type. Structure, primitives, and nullability stay the
  signature side's; only names come from metadata. Applied in both the plain and suspend member-walk
  arms. Tests:
  `tests/classpath_member_mutable_collection_e2e.rs`, `tests/classpath_property_mutable_collection_e2e.rs`.
- **A non-inlined `suspend inline fun` whose lambda argument itself SUSPENDS is DECLINED, not miscompiled
  (safety guard).** `kotlinx.coroutines.sync.Mutex.withLock` is `suspend inline fun <T> Mutex.withLock(owner:
  Any? = null, action: () -> T): T`. krusty does not splice it — it lowers the call as a plain
  `MutexKt.withLock$default(mutex, owner, Function0, cont)`, passing the lambda as a NON-suspend `Function0`.
  That is correct only when the lambda body does not suspend (`m.withLock { 42 }` compiles + runs; see
  `build840_nn1`). When the body suspends, a `Function0.invoke()` cannot legally call a suspend function, so
  the emitted closure is invalid bytecode — krusty exits 0 but `-Xverify:all` reports an operand-stack
  underflow. A non-suspend `() -> T` param whose lambda body suspends is only accepted by the front end
  because the callee is `inline` (an inline lambda inherits the caller's suspendability), so `ir_lower`'s
  resolved-extension path DECLINES the file when `c.suspend` and a non-suspend function-typed lambda argument
  suspends (`ast_body_suspends`, the AST-level suspend detector shared with the suspend-lambda classifier).
  Generic — keyed on the shape, not the `withLock` name (the `$default` synthetic's metadata `inline` flag is
  `None`). The real fix (general suspend-inline splicing: inline the lock/try/finally body and splice the user
  lambda into the enclosing CPS state machine, as kotlinc does) is future work; until then this guarantees a
  bail over a miscompile. Test: `tests/suspend_inline_hof_suspending_lambda_reject_e2e.rs`.
- **A fully-qualified top-level function call `a.b.helper(args)`.** The shared segment walk must end
  with prefix `a.b` committed as a package. The checker selects and records the exact callable/facade;
  lowering consumes that record and never parses the receiver spelling. Test: `tests/fq_toplevel_call_e2e.rs`.
- **A fully-qualified CONSTRUCTOR call via a package path `a.b.Ctx(x = 1, y = 2)`.** The prefix commits
  as a package and `Ctx` is one classifier edge. The checker records the selected constructor and
  result identity; lowering consumes those facts. Test: `tests/fq_ctor_call_e2e.rs`.
- **`break` / `continue` in EXPRESSION position (`val v = x ?: continue`, a `when` arm).** Kotlin's
  `break`/`continue` are `Nothing`-typed expressions (like `return`/`throw`), not only statements — new
  `Expr::Break`/`Expr::Continue` (parsed in `parse_prefix`, typed `Ty::Nothing`, `expr_diverges`), lowered
  to the same `IrExpr::Break`/`Continue` loop jump as the statement form. They are supported only in a TAIL
  position (an elvis RHS, an `if`/`when`-branch value, a block's trailing value), where the operand stack
  is empty at the jump; a `break`/`continue` used mid-expression (`x + break`, `inc() downTo continue`,
  `while (break)`) would jump with operand-stack values krusty's emitter doesn't clear, so
  `break_continue_tail_only` (a `lower_body` pre-scan) declines that body (skip, never miscompile). Test:
  `tests/break_continue_expr_e2e.rs`.
- **A default PARAMETER whose default VALUE is an object construction (`fun list(f: F = F(), n: Int = 2)`),
  called omitting that argument.** The `foo$default` synthetic stub re-emits an omitted parameter's default
  expression, so `toplevel_default_stub_safe` now ACCEPTS a plain `new`/object construction default (it was
  excluded alongside lambdas). A VALUE/inline-class construction default stays excluded — it erases to its
  unboxed underlying and mangles the owning function's name, which the plain static stub can't box/unbox
  (`default_expr_stub_safe` rejects a `New` of an `is_value` class, and an external value class via
  `external_value_classes`); such a file falls back to the inline call-site fill / skip. This is the
  AuditService root (a suspend service `list(filters = AuditFilters(), …)`). Test:
  `tests/construction_default_arg_e2e.rs`.
- **A `const val` inside an `object`.** Kotlin inlines every const read; krusty now does the same — a
  pre-scan records each literal-valued object `const val` in `object_const_lits[(object internal, name)]`,
  and a read inlines the literal (unqualified inside the object's own methods via `cur_class`, and qualified
  `Obj.NAME`). The const is emitted as a `public static final` + `ConstantValue` field on the object class
  (kotlinc's layout — `is_backing_field_prop` excludes const, so it is neither an instance field nor a
  `getX()` accessor). This removes the init-ordering hazard that gated such an object out; a computed
  (non-literal) const keeps the object gated. Test: `tests/object_const_val_e2e.rs`.

- **Reordered named arguments evaluate in SOURCE order (`f(b = X(), a = Y())`).** Kotlin evaluates
  arguments in written order, then binds each to its parameter position. When a reordering moves a
  SIDE-EFFECTING argument out of source order, `lower_args_defaulted` spills each argument to a fresh temp
  in source order (a `prelude` of `IrExpr::Variable` decls) and loads the temps in slot order for the call;
  the caller wraps the built call in `Block { stmts: prelude, value: call }` (via `wrap_arg_prelude`) so the
  temps live in the enclosing scope — a temp in a value-position `Block` used AS an argument would be scoped
  away before a later argument reads it (`Block` emit clones+restores `self.slots`). A pure reordering
  (const/name args, order-independent) keeps the byte-identical slot-order lowering (no prelude). Applies to
  top-level function and constructor calls. Test: `tests/named_arg_source_order_e2e.rs`.

- **Named arguments on a same-file MEMBER method / EXTENSION function (`z.test(b = …, a = …)`,
  `"x".ext(b = …, a = …)`).** The checker's member named-arg gate accepts any member with recorded
  parameter names (not only one with defaults). The lowerer reorders at the call site: `lower_named_member_call`
  (a `MethodCall`) and `lower_named_ext_call` (a static `Call` with the receiver as arg 0) evaluate the
  RECEIVER first, then each argument in SOURCE order into a temp, then load the temps in parameter (slot)
  order — matching Kotlin's left-to-right evaluation while binding labels to positions, wrapped in a
  `Block` so the temps outlive the call (as for the top-level path). A no-default user member/extension
  named call is ALWAYS handled or skipped, never routed to positional pairing (which would bind the labels
  in the wrong order). Parameter names for a no-default function are recorded in `fn_param_names`
  (previously only defaulted functions were). Overloaded members share one class-map slot (a pre-existing
  limitation); a divergent overload degrades to a skip via the `param_names`/`lower_arg` type checks, never
  a miscompile. Test: `tests/named_arg_member_e2e.rs`.
  The CHECKER type-checks a named member call against each argument's MAPPED parameter (via `map_call_args`),
  not positionally — otherwise a reordered argument bound to a differently-typed parameter (e.g. `c = { }`
  for a `() -> String` parameter reordered before the `String` parameters) would be checked against the
  wrong parameter type ("inferred type is Function but String was expected"). This `map_call_args` path now
  fires for a named call to a NO-DEFAULT method too (previously only defaulted methods), and falls through
  to the shared return-type logic so a generic higher-order member still infers its `<R>`.

- **Top-level default arguments via the `$default` synthetic (`fun f(a: String, b: String = compute())`,
  called `f("A")`).** krusty inline-fills CONST-literal defaults at the call site; for a NON-const /
  side-effecting default it now emits kotlinc's `f$default(realparams, int mask, Object marker)` synthetic
  (`emit_facade_default_stub`: no `self`, value-index `i` → slot `i`; for each `mask & (1<<i)` bit set it
  evaluates `default_i` into the slot then `invokestatic`s the real facade method) and routes an
  omitted-default call to it via `Callee::LocalDefault` (`lower_toplevel_default_call`: provided arguments
  evaluated in source order into temps, omitted slots get a zero placeholder + their mask bit, marker
  `null`). Gated by `toplevel_default_stub_safe` to a SOUND subset — an unmangled function whose default
  expressions are simple (no lambda, object/value-class construction, `invoke`, value-class-mangled call,
  or reference beyond the parameters), and no user function already named `<name>$default`. A value-class
  or lambda/wide-shape default falls back to the (unchanged) inline fill / skip, never a miscompile (this
  gate was added after an ungated version regressed value-class-parameter + lambda-default corpus files
  with `VerifyError`/`ClassCastException`). Test: `tests/default_args_synthetic_e2e.rs`.
  A default may reference an EARLIER parameter (`fun f(a: Int, c: Int = a + 1)`): it is realized inside the
  single `$default` synthetic where the parameters are in scope (the checker declares each parameter as it
  checks defaults, left-to-right). This is still rejected for an OVERLOADED function (its overloads share
  the name `foo$default` and the omitted-default routing isn't overload-aware — the checker and
  `toplevel_default_stub_safe` both count every same-name non-member function, so they agree). An omitted
  PRIMITIVE-typed default slot passes the primitive zero (`iconst_0`), not `null` — `zero_placeholder` maps
  a non-nullable boxed-primitive `Obj("kotlin/Int")` (a JVM `int`) to `0`.

- **Generic constructor type-argument inference (`Pair(1, 2)` → `Pair<Int, Int>`).** A classpath generic
  class constructed without explicit `<T>` previously erased to the raw type, so `first`/`second`/
  `componentN` typed as `Any` (breaking destructuring + arithmetic). `SymbolSource::infer_constructor_type_args`
  (JvmLibraries) unifies the constructor's generic parameter signatures (which name the class formals) with
  the actual argument types, binding each formal (unbound → `Any`); `ctor_result` applies it when no explicit
  type argument is present. Test: `destructure_e2e::classpath_generic_ctor_type_args_inferred`.

- **Numeric reduction extensions selected by element type (`List<Int>.sum()`, `average()`).** `sum`/
  `average` are `@JvmName`-mangled by the receiver's ELEMENT type — `List<Int>.sum()` is the bytecode
  method `sumOfInt(Iterable<Integer>): int`, `List<Long>.sum()` is `sumOfLong`, `average()` is
  `averageOfInt`. The Kotlin source name is not a JVM method, so ordinary extension resolution missed it
  (and the resulting `Error` cascaded into unrelated `require`/logger calls in the same function). The
  extension walk now derives the mangled name from the element's simple name (`<name>Of<Element>`, the same
  convention the `sumOf`-by-lambda-return path uses — `ty_simple_name` from the element's canonical internal
  name, no per-type list) and binds ONLY the candidate whose generic-signature receiver element equals the
  actual element (a no-argument overload — the same-named lambda `sumOf` has an extra parameter). Test:
  `collection_members_e2e::numeric_reduction_extensions_by_element_type`.

- **Non-inline top-level generic HOF binds the lambda parameter type (`transform(Item(…)) { it.name }`).**
  A user `fun <T, R> transform(x: T, f: (T) -> R): R` binds `T` from the first value argument, so the
  lambda parameter `it` types as that concrete type and `R` is inferred from the lambda body (the call
  result). The lambda materializes as an erased `Function1` whose `invoke` `checkcast`s its parameter —
  sound for a reference/class binding (as a non-generic HOF already does). `user_generic_call` previously
  applied only to `inline` HOFs; it now also handles a non-inline one. A SAME-MODULE `@JvmInline value
  class` binding is allowed too: the value crosses the erased boundary BOXED, and the declared-VC
  function-type machinery types the lambda parameter as the value class with a boxed slot + per-read
  unboxing (`tests/generic_hof_vc_binding_e2e.rs` — the corpus `unboxGenericParameter/*` bucket). A
  CLASSPATH value class or an unsigned type still stays erased (`value_underlying`/`is_unsigned` guard):
  their value-box unbox isn't modeled, so recovering the binding would miscompile.
  Test: `generic_fn_e2e::non_inline_generic_hof_binds_lambda_param`.

- **Java (non-Kotlin) static method calls, with overload selection (`Logf.make(x)`, `Logf.parse(s, 16)`).**
  A `.class`-read Java class's static methods land in the type's static list; the checker's class-name
  static-call path resolves the arity/type-appropriate overload via `resolve_companion` and now types the
  class-name receiver as its own `Obj(internal)` so the lowerer's classpath static-call path emits the
  `invokestatic` (previously the checker resolved it but the emit bailed). Test:
  `java_instance_e2e::calls_java_static_overloaded_methods`.

- **Integer-literal widening in overload resolution (`Instant.ofEpochSecond(1_700_000_000)`).**
  Overload resolution receives call arguments as
  `CallArgKind::{Typed, LambdaLiteral, IntegerLiteral}` (replacing the parallel
  `integer_literals`/`lambda_literals` flag arrays). `IntegerLiteral` carries the ordinary runtime
  type plus syntax-only constant provenance, so it can adapt an `Int` literal to `Long`, or to
  `Byte`/`Short` when the safely folded value fits. Provenance never enters `Ty`, signatures,
  generic bindings, checked expression types, IR, JVM descriptors, or LSP output. One shared AST
  recognizer serves both lightweight signature inference and the full checker: it accepts a literal,
  unary `-`/`+`, or an arithmetic constant expression over literals, so
  `1_700_000_000 + 1` widens like a bare literal. Folding uses checked `Int` operations because
  lowering evaluates the expression before applying the call-boundary coercion; overflow and division
  by zero therefore remain ordinary, non-adaptable `Int` expressions instead of being miscompiled.
  This also lets the lightweight signature inferer used during property signature collection infer
  properties initialized by JDK static factories or imported top-level overloads. The same inferer
  preserves EXPLICIT call type arguments (`val servers = mutableMapOf<String, JsonObject>()`):
  they bind the callee's type parameters directly, so the return-agreement probe — which resolves
  without them and erased `K`/`V` to `Any`, recording `MutableMap<Any, Any>` for any non-local
  property — is skipped for a call that carries them, and the generic path binds them instead
  (`call_targs_property_inference_e2e`). Tests:
  `classpath_jdk_static_e2e::top_level_library_calls_use_literal_origin_after_argument_mapping`,
  `classpath_jdk_static_e2e::jdk_static_call_return_type_inferred_for_private_property`.

- **Static call on a bare same-package (incl. ROOT-package) classpath class name (`J.greet()`).** Kotlin
  makes same-package declarations visible without an import, and the file's own package — the root
  package for an unpackaged file — is the first classifier import level. Static-call receivers,
  constructors, and type positions now use the same committed root selection and left-to-right
  qualifier walk. A lexical value or top-level property named `J` wins, and a failed member segment
  never reopens `J` as a package or classifier. Test:
  `java_source_interop_e2e::root_package_static_call_matches_other_positions`; corpus
  `fakeOverride/kt40180*.kt` exercise the sibling type positions.

- **`open`/`override` members are emitted WITHOUT `ACC_FINAL` (kotlinc's member modality).** The
  emitter's finality shortcut ("no same-module subclass ⇒ method is final") is unsound across a
  compilation boundary: kotlinc keeps an `open`/`override` (non-`final`) member's flag OPEN even when
  nothing in the module extends the class, because a separately compiled module — or javac in a mixed
  Java/Kotlin build — may override it. `FunDecl.is_open` (parser: `open`/`override` without `final`)
  flows to `ir.open_methods`, which the JVM backend already honors. Surfaced by the Kotlin-first
  Java-interop pipeline: javac rejected `class J extends A` because krusty's `A.name()` (an `open fun`)
  carried `ACC_FINAL`. Test: `java_source_interop_e2e::java_extends_kotlin_via_stub_pipeline`.

- **Java-source signature stubs (Kotlin-first mixed compilation, `docs/JAVA_INTEROP.md` slice 2).**
  When a box test's Java references Kotlin declarations, javac cannot run first. `jvm/java_stub.rs`
  parses the Java SIGNATURE surface (never bodies) and emits stub `.class` files — descriptors, access
  flags, generic `Signature` attributes; concrete bodies are `aconst_null; athrow` since a stub is
  never JVM-loaded. krusty compiles Kotlin against the stub dir, then javac compiles the real Java
  against krusty's output, and only javac's classes ship. Name resolution is callback-based (Kotlin
  module names + classpath probe) — an unresolvable type aborts stub generation (skip, never guess).
  Tests: `jvm::java_stub::tests` (unit), `java_source_interop_e2e::java_extends_kotlin_via_stub_pipeline`.

- **Member overloads with different erased signatures (`listIterator()` / `listIterator(Int)`).**
  `ClassSig.methods` (and the lowerer's `ClassInfo.methods`) hold per-name overload LISTS in
  declaration order; call sites select by argument types (`method_matching` mirrors the top-level
  `pick_overload`; `module_symbols` feeds every overload into the `SymbolResolver`; the lowerer
  pairs the i-th same-name AST decl with the i-th signature and resolves this-calls by arity with a
  base-chain walk). Sound-skip rules where erasure defeats selection: TRUE SIBLINGS (same owner)
  differing at a position where either side is an erased type variable resolve to nothing (kotlinc
  selects on the SUBSTITUTED types krusty erased away — `foo(x: T)` vs `foo(x: A<T>)`); an
  erased-`Any` ARGUMENT at a differing position likewise. Override CHAINS across owners keep
  most-derived-first order, erasure differences and all. Exact erased duplicates stay rejected
  (`ClassFormatError`). Tests: `member_overloads_e2e`; corpus
  `bridges/substitutionInSuperClass/*` stay sound skips.

- **`override` must override something (module-closed hierarchies).** With overloads, a same-name
  sibling of a different arity no longer pairs with a supertype method, so an `override` modifier
  is checked explicitly: it must match a supertype member by name + arity, else
  "'f' overrides nothing." (kotlinc's rejection). Enforced only when the hierarchy is MODULE-closed
  (`hierarchy_is_module_closed`) — a classpath supertype's members are invisible to the walk —
  with `kotlin/Any`'s `toString`/`hashCode`/`equals` exempt. Test:
  `resolver_errors_coverage_e2e::override_with_wrong_signature`.

- **Interface bridges have exactly two legitimate directions.** GENERIC-IFACE: the interface param
  is the erased `Object`, the impl concrete (`A<String>.foo(Object)` → `foo(String)`).
  FAKE-OVERRIDE: an ABSTRACT interface member with a concrete param satisfied by an inherited
  erased-generic impl (`Tr.hello(String)` over `Foo<T>.hello(Object)`, kt1939) — the bridge boxes
  a scalar param where needed (`emit_bridges` scalar→reference `valueOf`). But an interface method
  WITH A DEFAULT whose concrete param the impl merely erases (`B.foo(int)` next to `foo(t: T)`)
  is NOT overridden by it — the default stays live (kotlinc; KT-78321) — so no bridge may shadow
  it. Corpus: `bridges/kt1939.kt`, `defaultArguments/implementedByFake*.kt`,
  `reified/overrideResolution*.kt` all PASS.

- **A type parameter with multiple FUNCTION-TYPE bounds is rejected** (`where T : () -> Unit,
  T : (Boolean) -> Unit`): a `T` value would be convertible to several SAM shapes, and krusty's
  SAM conversion adapts lambda literals, not values behind an erased `T` — kotlinc synthesizes a
  wrapper krusty doesn't. Rejected in `parse_where_clause` (where-clause bounds are otherwise
  erased/discarded). Corpus: `funInterface/intersectionTypeToFunInterfaceConversion.kt` skips.

- **Multiplatform `expect`/`actual` (JVM model).** A platform module and its `dependsOn` chain
  compile as ONE source set (kotlinc's JVM MPP compilation): `split_modules` parses the full
  `name(dependencies)(friends)(dependsOn)` test-module header, the gate merges `dependsOn` sources
  transitively (dependency-first) into the platform module and never compiles a pure-`dependsOn`
  target standalone; FRIEND deps ride the classpath like regular deps. `strip_matched_expects`
  (frontend, gated on `+MultiPlatformProjects`) drops every top-level `expect` declaration matched
  by a non-expect counterpart — same kind + name, arity and extension-receiver name for callables —
  or by a TYPEALIAS for an `expect class` (`actual typealias S = String`). The `actual` modifier is
  inert; an unmatched `expect` stays and fails checking (skip, never mis-grade). `open`/`override`
  PROPERTY accessors carry `PropDecl.is_open` → `ir.open_methods` (non-final, the property analog
  of the member-modality rule). Interface bridges synthesize across FILES of one module: a
  cross-file module interface's erased signatures come from the symbol table (generic-iface
  direction only — body-presence isn't recorded there, so the fake-override direction stays
  same-file). Tests: `mpp_expect_actual_e2e`; corpus `multiplatform/` 75 PASS / 0 FAIL
  (box total 2744 → 2825).

- **Operator extensions on nullable PRIMITIVE receivers dispatch by call-site nullability.**
  `operator fun Int?.inc()`, `Long?.compareTo(Long?)`, `Int?.times(Int)` (the dispatchable set:
  `plus`/`minus`/`times`/`div`/`rem`/`compareTo`/`inc`/`dec`) are accepted and routed: a receiver
  statically typed `T?` has no builtin operator (it needs a non-null receiver), so the extension is
  the only applicable candidate; a non-null receiver keeps the builtin. Sound because
  `Ty::erased_recv` keys `Nullable(prim)` under the BOXED wrapper class — a non-null primitive
  operand can never produce that key, so the two never collide (this is exactly kotlinc's
  member-beats-extension applicability outcome). `x++`/`x--` dispatch on the variable's BINDING
  type (not a flow-narrowed use type): the update writes back to the (boxed) slot, and checker and
  lowerer must agree on the representation. `this != null` / `this == null` in the extension body
  narrows `this` to the unboxed primitive (a smart-cast scope entry that wins over the declared
  receiver type), so `this.inc()` / `this + 1` inside the body take the builtin — no
  self-recursion. Nullable REFERENCE receivers (`String?.plus`) and non-dispatchable operator
  names (`Int?.get`, `Int?.equals`) stay rejected: reference nullability is folded at call sites /
  those call paths never consult the key, so accepting them would silently keep the builtin — a
  miscompile. Tests: `nullable_receiver_operator_ext_e2e`; corpus `classes/kt72{3,5}.kt`,
  `increment/{postfix,prefix}NullableIncrement.kt`,
  `operatorConventions/compareTo/customCompareTo.kt` compile (box 2975 → 2982).
  `primitiveTypes/kt75{3,6,7}.kt` (bitwise/unary names) stay skipped on separate gaps
  (builtin `shl` return typing, safe-call with a primitive result), so those names stay rejected.

- **krusty-lsp resolves unbuilt Java sources through in-memory signature stubs.** The LSP collects
  sibling `.java` sources and injects lenient `jvm::java_stub` output into the analysis classpath.
  Lenient mode skips malformed declarations and erases unresolved member types; strict compiler
  callers still reject them.



- **JPS `packagePrefix` roots match imports through package-qualified logical paths.** A source
  root declaring `packagePrefix="org.example"` stores `org.example.p.X` at `<root>/p/X.java`; the
  LSP's import-driven Java loader matches import paths against `org/example/p/X.java` (prefix
  directories + root-relative path), so prefixed dependencies keep their budget priority
  (`crates/krusty-lsp/src/project/sources.rs::imported_java_sources_match_through_package_prefixed_roots`).

- **Java `...` parameters are varargs in stubs.** The signature stubs emit `ACC_VARARGS` for a
  trailing `Type... name` parameter (methods and constructors), so element-style calls
  (`h.reg("x", fix)`, zero-element `h.reg("y")`) and Kotlin spreads (`reg(s, *fixes)`) resolve
  against source-stubbed Java members exactly as against compiled ones
  (`src/jvm/java_stub.rs::java_varargs_parameters_emit_acc_varargs`,
  `crates/krusty-lsp/src/compiler_analysis.rs::source_set_spreads_kotlin_vararg_into_java_vararg_member`).

- **A generic static's return binds from the call arguments.** `<T extends Node> T
  copyOf(T, Document)` called with a `Node` returns `Node`, not the
  erased `Object` — the companion-member path binds the generic signature against the arguments
  exactly as instance members do (`tests/generic_static_field_e2e.rs`).

- **Member types resolve from their enclosing class chain in stubs.** A Java source referencing a
  sibling member type without qualification (`Proc` inside `class Builder { interface Proc {…} }`)
  resolves through the enclosing declarations (`Builder$Proc`) before the package, per JLS scoping —
  previously the reference silently erased to `Object` in lenient stubbing, so the nested SAM
  parameter never matched
  (`crates/krusty-lsp/src/compiler_analysis.rs::source_set_converts_sam_lambdas_on_implicit_receivers_and_nested_interfaces`).

- **SAM conversion works on implicit receivers.** A trailing lambda passed to a Java member of an
  implicit receiver (`Button().apply { addActionListener { … } }`) types against the functional
  method's parameters — including a lambda with no declared parameters (`it` bound) — and member
  selection receives the lambda-literal flags, exactly as on an explicit receiver (same test).

- **Explicit type arguments bind generic static SAM calls.** `Maps.create<String, Int> { s -> … }`
  seeds `K`/`V` from the call's type-argument list before any
  argument unification: the SAM lambda's parameter types substitute through (`s: String`) and the
  return types as `Map<String, Int>`, matching kotlinc
  (`crates/krusty-lsp/src/compiler_analysis.rs::source_set_binds_explicit_type_args_on_generic_static_sam_call`).

- **Interface fields are implicitly public static final (JLS §9.3).** Signature stubs stamp the
  implicit flags, so generic constant-holder fields (`Modifiers.STATIC`, `Names.STRING`) resolve
  as static field reads
  (`src/jvm/java_stub.rs::interface_fields_are_implicitly_public_static_final`).

- **All-caps Java getters map to decapitalize-smart properties.** `getID()` reads as `id`,
  `getURLPath()` as `urlPath` — the physical-getter fallback tries the re-uppercased leading-run
  spelling after the conventional `getX`
  (`crates/krusty-lsp/src/compiler_analysis.rs::source_set_maps_all_caps_java_getters_to_properties`).

- **Modifier-prefixed local functions parse in any body.** `tailrec fun`/`suspend fun` local
  declarations are statements everywhere, not only in scripts; the soft-keyword prefix no longer
  parses as an expression name (`src/frontend.rs::modifier_prefixed_local_functions_parse_in_bodies`).

- **Element-form vararg calls select and lower against classpath extensions.** `"a.b".trim('.')`
  expands `trim(vararg chars: Char)` element-wise (an exact element type beats an assignable one, so
  the `Char` overload wins over `String`); `fq.split('.')` additionally requires every parameter
  after the vararg to be defaulted and pairs the base's `$default` synthetic by parameter identity,
  with the lowering PACKING the elements into the array before the mask machinery. The selected
  callable carries its declared vararg index separately from its logical element type: for
  `fun <T> List<T>.render(vararg values: T, separator: String = …)`, a `String` specialization
  still occupies a physical `Object[]` slot, and positional arguments at that non-final vararg
  remain elements while `separator` defaults. Lowering therefore never rediscovers the slot by
  comparing logical and physical types; each element lowers to its specialized logical type and
  is then coerced to the physical array element, so primitive specializations are boxed for
  `Object[]` while primitive arrays remain unboxed
  (`tests/vararg_element_default_e2e.rs` — runtime-verified; both failures were VerifyErrors).

- **A plain constructor initializer types a capturable local.** `val sb = StringBuilder()` is
  capturable by an anonymous object exactly like an annotated local — the capture list infers the
  type from the capitalized bare-name constructor call, and the checker verifies the name like an
  explicit annotation; a function-call initializer (`val xs = listOf(…)`) stays uncaptured
  (skip-not-wrong) (`tests/anon_object_capture_e2e.rs::captures_constructor_initialized_local` —
  runtime-verified mutation visibility).

- **An anonymous object captures the INNERMOST binding of a name.** Capture discovery walks the
  scope tower innermost-first and keeps the first candidate of each name — the same binding a read
  at the construction site resolves to. That keeps smart casts visible inside the object body
  (`if (t != null) { object { fun g() = t.length } }`) and binds a shadowed local to its inner
  declaration (`tests/anon_object_capture_e2e.rs::captures_smart_cast_val` /
  `captures_inner_shadowed_local` — runtime-verified).

- **A Java accessor pair without `@Metadata` is a writable synthetic property.** `x.text = v` on a
  Java receiver resolves the write to the single-argument `void` setter named by Kotlin's accessor
  rules (`text` → `setText`, `isOpen` → `setOpen`) — but only when the getter also resolves
  (kotlinc synthesizes the property from the getter; a setter alone creates none), and never when
  the receiver has a real `@Metadata` property (a Kotlin `val` stays read-only even if a `setX`
  exists). Among setter overloads, the one whose parameter matches the getter's type wins; an
  ambiguous remainder resolves to none
  (`crates/krusty-lsp/src/compiler_analysis.rs::source_set_resolves_java_setter_backed_property_write`).

- **Member types of a Java interface or annotation are implicitly public (JLS §9.5).** The Java
  signature stubs emit `interface Registry { final class Handler {…} }` with `ACC_PUBLIC` on
  `Registry$Handler`, so `Registry.Handler.publish(…)` resolves like kotlinc
  (`src/jvm/java_stub.rs::interface_nested_types_are_implicitly_public`).

- **A Kotlin override of a Java-supertype getter refines the synthetic property's type.**
  `interface RefinedCatalog : JavaCatalog { override fun getEntries(): Array<RefinedEntry> }`
  keeps the Java synthetic property `entries` (the property exists because a JAVA base declares
  the accessor; a pure-Kotlin `getX()` still creates none), but reads as the most-derived SOURCE override's
  return — `catalog.entries` is `Array<RefinedEntry>`, not `BaseEntry[]`. Applied on both the checked
  tier (`resolve_external_inherited_property`) and the declaration-only tier
  (`DependencyPlatform::property_members`)
  (`crates/krusty-lsp/src/compiler_analysis.rs::source_set_refines_java_getter_property_via_kotlin_override`).

- **A qualified static call resolves nested types through in-scope outers.** `Outer.Nested.m(args)`
  where `Outer` is imported/in scope resolves the receiver chain to `pkg/Outer$Nested` (an in-scope
  type shadows a package path, as in kotlinc), then dispatches `m` as a static/companion member
  (`crates/krusty-lsp/src/compiler_analysis.rs::source_set_resolves_interface_nested_class_static_call`).

- **A static field's generic type comes from its `Signature`, not its erased descriptor.** A read of
  `Keys.CURRENT : Key<Document>` retains its arguments so a generic callee binds from it
  (`<T> T getData(Key<T>)` returns `Document`); a signature carrying free type variables falls
  back to the erased descriptor
  (`crates/krusty-lsp/src/compiler_analysis.rs::source_set_binds_generic_return_from_generic_static_field`).

- **A module-declared argument class reaches a library parameter through its source supertypes.**
  Library-member overload selection admits an argument whose supertype walk runs through MODULE
  declarations (`class V : Thread()` — or an anonymous object over a declaration-only Kotlin base —
  passed to `take(Thread)`): the platform oracle alone only walks classpath supertypes and cannot
  see source classes. Applied in the ordered applicability pass and the assignability pass via the
  module-first source federation
  (`crates/krusty-lsp/src/compiler_analysis.rs::source_set_passes_module_subclass_to_java_member_parameter`).
  CONSTRUCTOR resolution admits the same walk in its assignability pass (`class V : Visitor()` into
  `Holder(Visitor)`): `resolve_constructor_name` threads the resolver's source federation next to
  the platform oracle
  (`crates/krusty-lsp/src/compiler_analysis.rs::source_set_passes_module_subclass_to_java_constructor_parameter`).

- **Extensions from declaration-only source tiers resolve like in-prefix ones.** A call to an
  imported extension whose declaring file sits beyond the inferred prefix (LSP dependency modules)
  selects through the fallback platform seam and synthesizes the checked signature from the
  resolved overload — defaulted parameters included; only the emit-facade owner is unknown, which
  checking never needs (`src/frontend.rs::declaration_only_extension_calls_resolve_and_type`).

- **krusty-lsp reports project and analysis work through server-initiated work-done progress.**
  When the client advertises `capabilities.window.workDoneProgress`, the async engine opens one
  token for project loading or analysis, updates that token when the current work changes, and ends
  it when the command completes or the connection shuts down. Unsupported clients receive no
  progress traffic. Project warnings and errors continue to use the existing `ProjectFeedback`
  message path.

- **Classpath classifier visibility applies after name resolution.** Imported, aliased, indexed,
  and package-qualified references use the same accessibility check. Public package-qualified types
  and constructors resolve without imports, while inaccessible classifiers produce the diagnostic
  for the source spelling. Exposed-visibility diagnostics for public declarations remain unsupported.

- **The dev-mode dump renders AST and IR arenas flat and id-ordered, never as trees.** The AST and
  IR are index-based by design (nodes reference each other by `u32` arena ids), so a flat listing is
  the faithful view: every node appears exactly once, including nodes unreachable from any
  declaration, and each node's `Debug` output carries its child ids for navigation. A tree renderer
  would need a match arm per variant across the full node sets and would rot as variants are added.
  The LSP keeps replay inputs only under `--dev`, with one pass-wide byte budget shared by every
  module; the on-disk store separately bounds individual and aggregate output. Cache names are
  SHA-256 digests of full document URIs, so workspace paths and source names do not leak through the
  directory layout and same-named external files cannot alias. Tests: `src/dump.rs` cover the
  document shape; `crates/krusty-lsp/src/dump_cache.rs` covers identity, privacy, atomicity, and
  retention.

- **Source nesting is depth-bounded — degrade, never crash.** The checker and IR-lowering bound
  their expression recursion at 500 semantic nesting levels; the parser bounds its recursion at
  1000 entries per funnel — expressions (`parse_bp`, plus annotation arrays/nested values which
  recurse while a declaration prefix is parsed), types (`parse_type`: nested type parens
  `((((Int))))`, nested generic arguments), and statements/declarations (`parse_stmt` plus the
  class-like declaration parsers: nested blocks `while { while { … } }`, nested
  classes/interfaces/objects/enums), each of which recurses outside `parse_bp` and carries its
  own guard. Nested blocks reach the later passes as `Expr::Block` nesting (covered by their
  expression guards) and nested classes hoist flat, but a genuinely deep generic `TypeRef` tree
  that the parser admits has no demonstrated checker/lowering bound yet — the parser guard is
  the demonstrated contract for types; bounding the later passes' `TypeRef` recursion is a
  follow-up. For expressions, one semantic level costs up to
  two entries (a binary right operand plus a parenthesized re-entry), so the parser admits every
  shape the later passes admit at up to two entries per level; redundant nesting (doubled parens)
  spends entries faster and trips the parser first. Past its bound the parser emits
  `expression`/`type`/`statement`/`declaration` `nesting too deep`, skips the rest of the
  over-deep construct bracket-balanced (angle-aware in type position, so each enclosing
  type-argument frame finds its `>`; error recovery neither rebuilds the nesting for the later
  passes to recurse over nor unwinds with an `expected ')'`/`'}'` cascade), and yields an error
  node; the checker types an over-deep expression as `Error`; lowering bails. kotlinc has no fixed
  documented bound (it stack-overflows on pathological nesting); krusty deliberately trades
  acceptance of pathologically deep nesting for a guaranteed diagnostic on any thread's stack. A
  left-leaning binary chain (`a && b && c`) parses and checks iteratively and does not count
  toward the depth. The bounds are survivable on a default 2 MiB thread stack in unoptimized
  builds via same-thread stack growth (`src/wide_stack.rs`), applied PER RECURSION LEVEL in every
  guarded funnel — a single entry-point reserve was measured to overrun on one deep genuine
  nesting shape per pass (5–6 parser frames per paren level; `check_call`-sized checker/lowering
  frames per call level), and 5000-deep statement/class nesting SIGBUSed past one grown segment.
  Tests: `tests/deep_expression_nesting_check.rs` (400/700-operand chains, 400 and 1500 nested
  parens, 450-deep call chain, 5000-deep annotation arrays, 400/5000 nested type parens and generic
  arguments, 400/5000 nested `while` blocks, 300/5000 nested classes, and mixed local-class/init/
  loop recursion) and `tests/deep_expression_nesting_check_e2e.rs`
  (450-level `0+(…)` right-nesting through the checker and lowering, end-to-end).

- **Vararg spread arguments mixed with plain ones (`f(x, *a, y)`).** A call that mixes spreads and
  plain arguments packs ONE array through the platform spread builder, exactly as kotlinc does:
  `kotlin/jvm/internal/SpreadBuilder` for a reference element, `<Prim>SpreadBuilder`
  (`IntSpreadBuilder`, …) for a scalar one — `new`, then `addSpread(array)` / `add(element)` per
  argument in source order, then `toArray`. A SOLE spread (`f(*a)`) keeps its own path: the array
  goes through the platform array-copy helper plus a `checkcast`, which is what kotlinc emits and is
  cheaper than a builder. The IR carries this as one `Vararg` node with a `spreads` flag per element,
  so the JS backend renders the same node as a native array literal with `...` spreads. Only a
  top-level single-`vararg` callee declared in the same file is lowered; any other callee still
  skips the file rather than risk a mis-pack, because the other vararg-packing paths ignore the
  spread flag. Tests: `tests/feature_coverage_v_e2e.rs::vararg_spread_forwarding`,
  `tests/resolve_parse_deep_coverage_e2e.rs::spread_operator_into_vararg`,
  `tests/feature_coverage_p_e2e.rs::vararg_named_and_spread_in_middle`,
  `tests/backend_rejection_coverage_e2e.rs::mixed_spread_vararg_accepted`,
  `tests/ir_lower_bail_coverage_e2e.rs::leading_fixed_then_string_spread_accepted`.
- **`when` on an unsigned subject.** Unsigned `==` is BIT equality — identical to the signed compare
  on the underlying `int`/`long`, for `UInt` and `ULong` alike, since magnitude never enters an
  equality test (`when (u: ULong) { ULong.MAX_VALUE -> … }` matches on the bit pattern `-1L`). So a
  `when` whose arm conditions are all unsigned literals lowers exactly like a signed one. An `in`
  test still skips the file: unsigned ORDERING differs from the signed compare above `Long.MAX`, and
  an unsigned `const val` comparand is not materialized yet. Test:
  `tests/feature_coverage_i_e2e.rs::unsigned_in_when`.
- **An unsigned integer literal takes its EXPECTED type.** Kotlin types an integer literal from its
  context, unsigned exactly like signed: `val a: UByte = 200u` is a `UByte` the same way
  `val b: Byte = 100` is a `Byte`, and `val c: ULong = 7u` is a `ULong` (verified against the reference
  `kotlinc`, which folds them to `bipush -56` / `ldc2_w 7L`). Absent an unsigned expected type the
  literal is `UInt`; the parser already promotes anything above `UInt.MAX` to a `ULongLit`. A magnitude
  that does NOT fit the expected type keeps `UInt`, so the ordinary initializer-mismatch diagnostic
  reports it rather than the value silently truncating. The stored constant is the bit pattern of the
  magnitude in the expected type's REPRESENTATION (`200u` as a `UByte` is the byte `-56`). Test:
  `tests/feature_coverage_i_e2e.rs::unsigned_literal_takes_the_expected_type`.
- **`UByte`/`UShort` operate as `UInt`.** Their representation is the SIGN-extended `byte`/`short` the
  JVM loads, so every widening out of it masks first (`UByte.toInt()` is `iand 0xFF`, `UShort.toInt()`
  is `iand 0xFFFF`) — exactly kotlinc's lowering. Kotlin gives them no arithmetic of their own: each
  operator is defined as `toInt()` followed by the `UInt` operator, so `UByte + UByte` is a `UInt`, and
  `/`/`%`/`<`/`>` route through the `UInt` platform helpers on the masked operands. `==`/`!=` stay on the
  narrow representation — equality is BIT equality, identical either way. `toByte()`/`toShort()` are the
  raw reinterpret (`200u.toByte()` is `-56`), so they emit nothing.

  Two consequences of computing in the int category, each of which cost a miscompile before it was
  pinned by a test. (1) The widened value must be carried as an `Int`: the emitter types a
  `PrimitiveBinOp` from its LEFT operand, so the mask node inherited the narrow `byte`/`short` and any
  consumer that BOXED it reached `Byte.valueOf` — which throws above 127 — or `Short.valueOf`, which
  silently wraps to a negative. (2) `inc`/`dec` must truncate BACK with `i2b`/`i2s` (kotlinc emits
  `iadd; i2b`), or the result leaves the canonical representation and stops comparing equal under the
  bit equality of (1) — `(127u as UByte).inc() == 128u.toUByte()` was false. Tests:
  `tests/feature_coverage_i_e2e.rs::{ubyte_and_ushort, ubyte_and_ushort_arithmetic_promotes_to_uint,
  ubyte_and_ushort_comparison_is_unsigned, ubyte_and_ushort_conversions,
  ubyte_and_ushort_interpolate_unsigned, widened_ubyte_and_ushort_box_as_int,
  ubyte_and_ushort_inc_dec_wrap_in_representation}`.
- **A sub-`Int` library constant inlines as its OWN narrow constant.** `Byte.MIN_VALUE`,
  `Short.MAX_VALUE`, `Char.MAX_VALUE`, `UByte.MIN_VALUE` … all read back from the classpath as an integer
  `ConstantValue`, but the constant's TYPE is the narrow one. Emitting `IrConst::Int` boxed them to
  `Integer` in a vararg or erased-generic position, where `Intrinsics.areEqual` compares WRAPPER CLASSES —
  so `x.id() != Byte.MIN_VALUE` (with `fun <T> T.id() = this`) was true for `x == -128`. Surfaced by
  `codegen/box/evaluate/intrinsicConst/incDec.kt` once the `UByte`/`UShort` emit block-list was lifted and
  the corpus stopped skipping that file.
- **A deferred `var` body property.** `class C { var x: String }` — declared with a type and no
  initializer, assigned in an `init` block or a constructor body — is the same backing-field shape as
  a deferred `val`, plus the setter the plain property path already emits. A `var` with NO assignment
  on any path is well-formed only when an earlier initializer DIVERGES (`val t: String = TODO()`
  makes the remaining initialization unreachable, which is why kotlinc accepts it); the field is
  emitted and `<init>` throws before any store. Test:
  `tests/diverging_init_e2e.rs::diverging_property_initializer_runs`.
- **A `const val` of a NESTED object read through its outer class (`Registry.Const.MAX`).** A const
  read inlines its literal at the use site (kotlinc emits `ldc`, never a `getstatic`). Nested
  declarations are flattened under their dotted name, so the qualifier is matched as a whole dotted
  CHAIN of plain names rather than a single one — otherwise only a top-level `Obj.CONST` inlined and
  the nested form skipped the file. Test:
  `tests/resolve_parse_deep_coverage_e2e.rs::nested_class_qualified_access`.
- **A `vararg` parameter that is not last on an `inline fun`.** `inline fun pick(vararg xs: Int, f:
  (Int) -> Boolean)` is the idiomatic shape for a trailing lambda after a vararg. At the splice, the
  parameters before the vararg bind by index, the parameters after it bind from the END of the
  argument list, and the vararg absorbs the variable-width middle span. Named arguments around a
  non-last vararg are not modeled and still skip. Test:
  `tests/inline_vc_suspend_coverage_e2e.rs::inline_vararg_param`.
- **`return <suspend call>` inside a statement `when` arm.** `suspend fun pick(n: Int): Int { when (n)
  { 0 -> return a(); else -> return c() } }` — the CPS flattener models a suspending `Variable` init,
  not a suspending `Return`, so each such `return` is desugared to `val tmp = <call>; return tmp`
  first. That rewrite now descends into statement-position `when` arms and nested blocks instead of
  walking only the body's top-level statements; a `Lambda` body is a separate state machine and is
  never descended into. Test:
  `tests/feature_coverage_s_e2e.rs::suspend_when_returns_from_multiple_arms`.
- **Relational operators on a `Comparable` whose `compareTo` is not a declared source member.**
  `a < b` desugars to `a.compareTo(b) < 0`. A source class declaring `operator fun compareTo` already
  drove this; a type whose `compareTo` comes from the CLASSPATH did not, because both the checker's
  and the lowering's classpath path sat under an `Obj`-internal-name lookup. `String` is a `Ty` of its
  own with no object internal name, so `"apple" < "banana"` fell through to the primitive comparison —
  the checker reported `operator cannot be applied to 'String' and 'String'`, and forcing it past the
  checker produced a `VerifyError` on a reference operand. Both sides now resolve `compareTo` through
  the library set and emit that member, comparing its `Int` result with 0. `Ty::String` is admitted
  alongside `Ty::Obj` by the ONE selected-target block, which records the callable in
  `resolved_operator_calls` — the single map lowering reads. The right operand must be a reference: an
  erased `Comparable<T>.compareTo` takes `Object`, so a primitive argument would need a box this path
  doesn't apply; for a `String` left operand it must be a `String` too, because resolving through the
  erased `compareTo(Object)` accepted `s < any`, which kotlinc rejects with "argument type mismatch:
  actual type is 'Any', but 'String' was expected". Tests:
  `tests/feature_coverage_v_e2e.rs::string_chunked_and_compare`,
  `tests/relational_compare_to_seam_e2e.rs::relation_with_non_comparable_right_operand_is_rejected`.

  A source `enum class` compares the same way, through the `compareTo` it INHERITS from
  `java.lang.Enum` — which no member lookup on the enum itself reports, so it is resolved on the
  SUPERTYPE. The parameter is the erased `Enum`, so the right operand is cast to it. krusty emits
  `invokevirtual java/lang/Enum.compareTo(Ljava/lang/Enum;)I` where kotlinc emits a `checkcast` plus
  `invokevirtual <E>.compareTo(Ljava/lang/Enum;)I`; both dispatch to the same method, and this matches
  what krusty already emitted for an EXPLICIT `a.compareTo(b)` on an enum. That supertype resolution
  is a FALLBACK, reached only when the selected-target block above found nothing — with kotlin-stdlib
  on the classpath the enum's `Comparable` supertype carries the member and the ordinary path wins; an
  empty classpath resolves `Comparable` from the builtins fallback, which does not. The fallback
  records its target in `resolved_operator_calls`, the same map as every other relational target.
  Recorded in `resolved_calls` instead — which lowering never consults for a relation — the checker
  typed the comparison `Boolean` while lowering fell through to the primitive `if_icmp*` on two enum
  references: a class file that compiled and then failed to load with `VerifyError: Bad type on
  operand stack`. Tests: `tests/feature_coverage_r_e2e.rs::enum_comparison_ordering`,
  `tests/relational_compare_to_seam_e2e.rs::enum_and_string_relations_run_without_stdlib`.
- **A BOUNDED type parameter's return, inferred at the call site.** `fun <T : Number> id(x: T): T`
  called as `id(3)` types as `Int`, not the erased bound `Number`. Two halves had to meet: the
  checker declined a bounded return outright ("an erased-return coercion is not modeled"), and the
  lowering's coercion of an erased call result required the PHYSICAL return to be erased-top — which a
  bound is not — so the boxed value stayed on the stack where an `int` was expected (a `VerifyError`).
  The unbox is now emitted whenever the erased return is a type parameter AND the physical return is a
  REFERENCE; that last condition matters, because the same coercion hook is reached with a primitive
  physical return (a defaulted `Char` parameter) where unboxing is wrong. Same shape as kotlinc:
  `Integer.valueOf` per argument, then an unbox of the result. The existing soundness guards on
  non-inline inference — unambiguous binding, no conflicting witnesses — are unchanged. Tests:
  `tests/feature_coverage_n_e2e.rs::bounded_type_param_comparable`,
  `tests/feature_coverage_x_e2e.rs::generic_fn_with_comparable_bound`,
  `tests/bounded_type_param_e2e.rs::comparable_operator_bounded_generic_called_with_primitive_runs`.
- **`EnumName.entries`.** Kotlin 2.x's replacement for `values()`. The emitter already synthesized
  the `$ENTRIES` field and its `getEntries()` accessor on every enum class (that is what kotlinc's
  byte-parity requires); only the READ had no resolution. The checker types `E.entries` as
  `EnumEntries<E>` — exactly what kotlinc types it — and `EnumEntries<E>` IS-A `List<E>`, so `size`,
  `[0]` and `for (x in …)` resolve through ordinary supertype member lookup. Resolution goes through
  ONE path for every enum: the classifier's semantic identity, then the `enum_entries_accessor`
  capability its symbol provider advertises, recorded as `ExprLowering::EnumEntriesRead` and consumed
  verbatim by lowering. An enum declared in the file being compiled is reached through that same
  provider seam (`ModuleSymbols` publishes the synthetic accessor for module enums), so `entries` has
  no source-origin branch on either side: a second checker arm that typed only `syms.enums`-backed
  receivers as `List<E>` shadowed the provider arm, left no recorded lowering, and made lowering fall
  through to evaluating the bare classifier receiver as a value (`expr Name` bail). Lowering emits the
  same `invokestatic <E>.getEntries()Lkotlin/enums/EnumEntries;` kotlinc does. The sibling synthetic
  members `values()`/`valueOf()` are NOT yet on this seam — they still gate on `syms.enums` and record
  no lowering; converting them is separate work. Tests:
  `src/resolve.rs::tests::source_enum_entries_records_the_declaring_owner_and_its_accessor`,
  `tests/feature_coverage_a_e2e.rs::enum_entries`,
  `tests/feature_coverage_r_e2e.rs::enum_reflection_members`,
  `tests/feature_coverage_x_e2e.rs::enum_rich_members`,
  `tests/nested_enum_access_e2e.rs::enum_entry_and_entries_property_from_another_source_file_use_the_declaring_owner`.
- **`this@Inner` — a nested class's own qualified-this label.** The enclosing chain (`this@Outer`
  from an `inner class`) already resolved; the class's OWN label did not, because a nested declaration
  is flattened under its dotted name (`Outer.Inner`) and that dotted string was pushed as the label,
  while a Kotlin label is always the SIMPLE name. Test:
  `tests/feature_coverage_p_e2e.rs::qualified_this_in_nested_class`.
- **`import Obj.CONST` — a `const val` imported from an `object`.** The import form already bound
  FUNCTIONS of an object (`import Config.greeting`), and the qualified read `Config.NAME` already
  worked; only the imported bare name did not, because the import-to-property lookup accepted a
  COMPANION owner alone. A companion's statics are hoisted onto the outer class and a plain object
  owns its own, so both spell the same static read and now share one lookup. A `const val` in an
  object is a real `public static final` field on the object class (the JVM realization of `const`),
  which is what makes this the ordinary static read; a NON-const object property is an instance field
  on `INSTANCE` and is deliberately not matched, since it needs the singleton receiver. Test:
  `tests/resolve_parse_deep_coverage_e2e.rs::import_object_member`.
- **An unqualified read of an INHERITED member (`name` inside an enum method).** `this.name`
  lowered; the bare `name` did not. The implicit-`this` read tried the class's declared properties,
  then an enclosing class's through `this$0`, and gave up — while the extension / receiver-lambda
  branch beside it already ended in the general "same path a qualified `this.n` takes" fallback. The
  in-class branch now ends there too, so a member inherited from a supertype (`name` / `ordinal` from
  `java.lang.Enum`, and any classpath supertype's) reads unqualified. Test:
  `tests/feature_coverage_x_e2e.rs::enum_rich_members`.
- **A `where` generic-constraint clause.** `fun <T> label(x: T) where T : Named` declares the same
  constraint as the inline `<T : Named>` form — Kotlin offers both spellings, and the second is
  REQUIRED once a parameter has more than one bound. The clause was parsed for its diagnostics and
  then discarded, so a `where` bound resolved no members at all while the inline form did. The pairs
  now join the declaration's `type_param_bounds`, so erasure and member resolution see one list
  regardless of spelling. Applies to functions, classes and interfaces alike. Test:
  `tests/feature_coverage_n_e2e.rs::where_clause_single_bound`.

  MULTIPLE bounds on one parameter (`where T : Comparable<T>, T : Named`) were initially left open
  and have since landed — see "A type parameter carries every bound" below. Kotlin gives
  such a parameter the INTERSECTION of its bounds and resolves members from all of them, while the JVM
  erasure takes ONE; at the time `Ty::TyParam` carried a single bound, so only that one's members
  resolved. An attempt to carry the later bounds by re-tagging the parameter's identity
  (`Ty::TyParam(name, first_bound)` in place of the bare erasure) was REVERTED: the checker and lowerer
  match structurally on `Ty::Obj` in many places, so a parameter that stops being an `Obj` stops
  resolving source-declared members (`resolve.rs`'s `matches!(rt, Ty::Obj(..))` module-member gate) and
  stops being assignable to its own bound — both shapes that worked before. The fix that landed keeps
  the extra bounds BESIDE the untouched erasure, not in a re-tagged one. Note also that the erasure is NOT
  simply the first declared bound: kotlinc hoists a CLASS bound ahead of interface bounds regardless of
  order (`where T : Named, T : Base` erases to `Base`), and writes `<T extends Base & Named>` in the
  generic signature where krusty writes only the first.
- **`ClassName.Companion` named explicitly.** The bare `ClassName` already denotes the companion
  singleton in a value position (`val f: Factory = Widget`); both spellings mean the same object, so
  they resolve to the same type and lower to the same `getstatic C.Companion:LC$Companion;`. As in
  the bare-name form, only a companion that DECLARES a supertype gets a registered `C$Companion`
  ClassSig — a plain companion is not a first-class value. Test:
  `tests/feature_coverage_r_e2e.rs::companion_implementing_interface`.
- **A companion's members in scope through the class body.** `class C { companion object { fun tag()
  … }; fun describe() = tag() }` — an INSTANCE member calls a companion function unqualified. Kotlin
  puts a companion's members in scope throughout the class body, so this binds the same static a
  qualified `C.tag()` does, and emits the same shape: `getstatic C.Companion; invokevirtual
  C$Companion.tag()`. A same-named INSTANCE and companion method may coexist when their accepted
  argument-count ranges do not overlap: the companion fallback only claims an arity its signature
  accepts, then the ordinary implicit-instance receiver gets a chance. Arity is the shared callable
  shape (defaults lower the minimum; a vararg removes the maximum), not raw parameter-vector length.
  An overlapping pair remains conservatively rejected because the current lexical lookup cannot yet
  rank two families that both accept the unqualified call without risking the companion owner winning
  inside an instance member. A companion `var` is admitted too — the same static backing field on the
  outer class a companion `val` already uses. Tests: `tests/companion_e2e.rs` (non-overlapping default
  and vararg shapes) and `tests/resolve_parser_diag_coverage_e2e.rs` (overlap guards), plus:
  `tests/feature_coverage_r_e2e.rs::companion_member_unqualified_from_instance`.

  A companion `var` is also WRITTEN through the class name (`C.created = 3`). The receiver is a
  CLASS NAME, not a value, so the checker resolves the target through the same `static_props` the read
  uses instead of typing the receiver as an expression — a class whose companion is not a first-class
  value would otherwise be reported unresolved. Two emitter facts follow from `var`: an owner-scoped
  static drops `ACC_FINAL` (a `putstatic` on a final field outside `<clinit>` is an
  `IllegalAccessError`), and a static that declares an owner is written directly on that class rather
  than through the facade's accessor/bridge path. Test:
  `tests/resolve_parse_deep_coverage_e2e.rs::companion_member_from_instance`.
- **`::prop.isInitialized`.** It reads as a property of a property REFERENCE, but kotlinc compiles it
  to a NULL CHECK on the backing field — a `lateinit` field holds `null` until assigned — so it needs
  no reflection and materializes no `KProperty` value. The obstacle is that every ordinary read of a
  `lateinit` field carries the throw-if-null guard, which is the opposite of what this tests; a
  dedicated IR node supplies the RAW read, and lowering builds the comparison from the ordinary
  comparison node so the branch/stackmap shape stays the one every other comparison uses. Tests:
  `tests/feature_coverage_v_e2e.rs::lateinit_and_isinitialized`,
  `tests/implicit_this_callable_ref_e2e.rs::lateinit_is_initialized_runs` (the box-corpus case).
- **One bytecode offset, one frame — merged across every label bound there.** Several labels can share
  an offset: a loop's `end` and the following statement's head, or `next`/`end` in an all-diverging
  `when`. Only one StackMapTable entry exists for that offset, and it must hold on EVERY edge reaching
  it, so the frames are merged — locals become their common prefix, everything past the first
  divergence reverting to `top`. Keeping the first (a plain dedup) claimed a local a later edge did not
  have: `for (v in 0 until 2) t += v` immediately followed by `while (t > 100) t -= 1` bound the `for`'s
  end and the `while`'s head at one offset, the emitted frame still named the `for`'s SYNTHETIC index,
  the `while`'s own back edge chopped it, and the back edge became narrower than its own target —
  "Inconsistent stackmap frames", on a program kotlinc accepts.

  The synthetic slots never appear in the LocalVariableTable, so the SAME/CHOP chain in the
  StackMapTable is the evidence, not the LVT. This retires the blanket rejection of `Array(n) { … }`
  with an array element, which was only removing the ARRAY route into the same defect: a 2-D array is
  built through a fill loop, and any statement between the two loops (even an `if`) hid it. Tests:
  `tests/feature_coverage_h_e2e.rs::adjacent_loops_verify`,
  `tests/feature_coverage_h_e2e.rs::two_dimensional_arrays`.
- **A companion `var` is written only within the file that declares it.** `ir.statics` holds the
  statics of the file being lowered, and the IR has no external static STORE (`ExternalStaticField` is a
  read), so a cross-file write declines with a named bail. The cross-file READ works, and the checker
  accepts the write — mutability is a symbol-table fact, so it is not misreported as
  `val cannot be reassigned`. Test:
  `tests/backend_rejection_coverage_e2e.rs::cross_file_companion_var_write_declined`.
- **A package-level `const val` reached by name (`import kotlin.math.PI`).** A `const` has no
  accessor, so it is absent from the property namespace — which models properties by their accessors —
  and the import bound nothing while `import kotlin.math.sqrt` (a function from the same package)
  worked. Which artifact holds the constant is a PLATFORM fact, so the platform answers with the field
  (`kotlin/math/MathKt.PI`) and the ordinary external-static-field path inlines its `ConstantValue`,
  which is what kotlinc emits at every use site. Test:
  `tests/resolve_parse_deep_coverage_e2e.rs::import_top_level_math`.
- **A COMPUTED property of a value class (`Result.isSuccess`) — still open, and why.** The same shape
  as the `const val` above: a `@JvmInline value class`'s non-constructor `val` has NO instance accessor
  at all — kotlinc compiles its getter to a static `<getterName>-impl(<carrier>)` — so the
  accessor-modelled property namespace never surfaces it and every read is "unresolved reference".
  Publishing such properties as zero-argument members under their source name (the same
  receiver-as-first-JVM-argument shape the value class's own FUNCTIONS already use) resolves and runs
  them, but it also makes two box-corpus cases reach a SEPARATE, pre-existing defect and MISCOMPILE:
  a value-class value passed through a `fun interface` method (`ResultHandler<T>.onResult(Result<T>)`)
  is handed over as the raw carrier where the erased interface descriptor expects the BOX, so the
  callee's `checkcast` throws. That defect is reachable without this feature (any `Result` argument to
  such a method), it simply has no corpus case that reaches it today. The property support therefore
  waits on the value-class boxing at an erased interface-parameter boundary; until then a read stays
  unresolved rather than compiling into a `ClassCastException`.
- **A constructor parameter of RECEIVER function type on a compiled class.** `Base(init: Cfg.() ->
  Unit)` erases to `Function1` in both the JVM descriptor and the `Signature` attribute, so only
  `@Metadata`'s `@ExtensionFunctionType` mark distinguishes it from `(Cfg) -> Unit`. Members and
  top-level callables already restored that mark, but a CONSTRUCTOR is absent from `@Metadata`'s
  function records — it lives in the constructor records, which krusty decoded for names/defaults only.
  Those records now also carry the per-parameter receiver mark, and a `<init>` member republishes it as
  the parameter TYPE and on its call signature, so a lambda argument binds `this` and a bare member
  call inside it resolves. Tests: `tests/classpath_ctor_receiver_lambda_e2e.rs`.
- **An integer argument in a WIDER primitive constructor parameter.** `Row(a: String, b: Long)` called
  as `Row("x", 1)`. krusty admits primitive widening at every call site (the emit site materializes the
  conversion), but constructor selection measured arguments by SUBTYPING alone, so any constructor with
  a `Long`/`Double`/… parameter was unreachable from an integer literal. Both constructor origins now
  apply the widening, and each keeps it as the LAST applicability pass so an exact-parameter overload
  still binds first; source-constructor selection additionally prefers the exact-type matches, since
  subtyping relates neither `Int` to `Long` nor back and could not otherwise separate them. Tests:
  `tests/ctor_numeric_widening_e2e.rs`.
- **A FULLY-QUALIFIED call to a vararg function (`kotlin.collections.listOf(1, 2, 3)`).** A vararg
  callee packs every trailing argument into ONE array parameter. The fully-qualified path paired
  arguments with parameters index-for-index, so the first element was measured against `Array<Any>`,
  and the lowerer skipped the shape outright. The checker now recovers the vararg slot from the
  candidate it selected, checks the packed arguments against the array's ELEMENT type (an explicit
  spread keeps the array type), and records the slot on the resolved callable so the lowerer packs the
  same arguments. Tests: `tests/fq_vararg_call_e2e.rs`.
- **A LABELLED trailing lambda and the local return it names (`run outer@{ … return@outer v … }`).**
  Two facts. Syntactically, a `label@` may precede a trailing lambda; the parser did not attach such a
  `{ … }` to the call, so the callee stayed a bare name ("unresolved reference 'run'"). Semantically, an
  explicit label REPLACES the implicit one (the callee's own name) that a `return@…` inside the body
  targets. A labelled return is LOCAL to its lambda, so lowering must model it per splice route: the
  receiver-less `run { … }` splice wraps the body and routes the return through a result slot, and the
  `forEach { … }` splice — which becomes a for-each LOOP — routes it to that loop's `continue`. A label
  that reaches neither, on a route that does not model it, now SKIPS the file: the previous
  fall-through emitted a real return out of the enclosing function, which the JVM verifier rejects at
  class load. Tests: `tests/labeled_lambda_return_e2e.rs`.

  A labelled lambda that is a VALUE rather than an argument (`val f = lbl@{ x: Int -> … return@lbl a
  … }`) is never spliced, so its label IS the closure method's own return scope and the closure route
  serves it directly. Such a lambda withholds its splice form: the same return node, spliced, would be
  a non-local return of the enclosing function carrying the wrong type. Test:
  `tests/labeled_lambda_return_e2e.rs::a_standalone_labelled_lambda_returns_locally`.

  Still open: a labelled return from a stdlib HOF whose lambda is routed through the bytecode splicer
  (`xs.sumOf tag@{ … return@tag 0 … }`). Withholding the splice form leaves that route no body to
  inline, and the closure fallback does not reach it, so the file skips.
- **An `open` property is read and written through its ACCESSOR, even inside the declaring class.**
  A subclass `override val`/`var` replaces the base's `get<Name>()`/`set<Name>()`, never the base's
  own private backing field, so a `getfield`/`putfield` from a base member would touch the base's
  storage and silently bypass the override. kotlinc emits `invokevirtual get<Name>()` for exactly
  this reason. A FINAL property keeps the direct field access; so does a PRIVATE one, which has no
  synthesized accessor to call (`private open` is not valid Kotlin, so this only decides what an input
  kotlinc rejects compiles to). A constructor's property INITIALIZER stays a `putfield` in both
  compilers — the field must be stored before any subclass accessor could run — while an `init { }`
  assignment to an open `var` goes through the setter, again as kotlinc does. A `val` has no setter at
  all, so the deferred initialization Kotlin permits for one (`open val c: B` assigned in `init { }`
  under `-ProhibitOpenValDeferredInitialization`) stays a `putfield`; every write rule is therefore
  conditioned on the property being a `var`.

  This holds only if EVERY access path applies it, and the paths do not share one implementation: a
  bare `name` read/write and an `x++` go through `ir_lower::open_source_property`, a qualified
  `this.name` through `jvm::ir_emit::direct_field_access`, keyed on `IrProperty::is_open`. That flag
  must therefore be set for a PRIMARY-CONSTRUCTOR property as well as a body one — both forms are
  overridable, and a review found the two sites disagreeing for the constructor form, so a bare write
  in a base member silently stored into the base's own field. It replaces the whole-file
  `gate:base-reads-override-internally` bail, which used to skip any class whose base read an
  overridden property. Tests: `tests/class_body_e2e.rs::open_property_virtual_dispatch`,
  `::open_property_virtual_dispatch_through_a_grandparent`,
  `::open_property_writes_and_constructor_declarations_dispatch_virtually`,
  `::open_var_init_block_writes_through_the_setter`.
- **A `when` subject compares against a BOXED primitive comparand.** `when (x: Any) { 1, 2, 3 -> … }`
  is valid Kotlin: `Int` is a subtype of `Any`, so the comparison can be non-trivially true, and
  kotlinc emits `Intrinsics.areEqual(x, Integer.valueOf(1))`. Comparability therefore tests the
  subject and the comparand in their REFERENCE forms (a primitive boxes to its Kotlin class, `String`
  names `kotlin/String`), and lowering boxes the comparand instead of rejecting the mixed
  primitive/reference compare. The converse — a primitive subject with a reference comparand
  (`when (i: Int) { null -> … }`) — has no such form and is still refused. Two comparand kinds keep
  bailing in LOWERING (the comparability rule above is unconditional, matching kotlinc): an unsigned
  one boxes to its own inline class rather than a plain wrapper, and a FLOAT/DOUBLE one compares by
  IEEE `==` whenever the subject is a primitive, which `Double.equals` is not (`-0.0 != 0.0`,
  `NaN == NaN`) — which of the two applies turns on whether an earlier `is` arm smart-casts the
  SUBJECT to the primitive, per-arm narrowing the lowering does not model (corpus case
  `ieee754/smartCastOnWhenSubjectAfterCheckInBranch_properIeeeComparisons.kt`). Tests:
  `tests/feature_coverage_p_e2e.rs::when_comma_conditions_and_mixed_is_in`,
  `::when_widened_subject_boxes_every_primitive_comparand`.
- **`x in a..b` over a WIDENED value.** `when (x: Any) { in 4..10 -> … }` compiles: kotlinc lowers it
  to `CollectionsKt.contains(4..10, x)`, and an `IntRange` is not a `Collection`, so that walks the
  range comparing with `equals` — true exactly when `x` is a BOXED element of the range. krusty keeps
  its comparison chain and guards it with the `instanceof` that fact implies (`x is Integer &&
  4 <= x.intValue() <= 10`). The guard must short-circuit, so it is a branch, not the eager `iand`:
  unboxing a value of another class would throw. A value type unrelated to the boxed element
  (`x: String in 4..10`) is still rejected. The widened form is `Iterable<T>.contains`, so only
  `Int`/`Long`/`Char` elements qualify: a floating-point range is a `ClosedFloatingPointRange`, not an
  `Iterable` (kotlinc rejects `x: Any in 1.0..2.0` outright); a `Byte`/`Short` range is really an
  `IntRange`, whose elements box to `Integer` rather than the bound's own wrapper; and an unsigned
  range's elements box to their inline class, which krusty erases to the signed primitive. Tests:
  `tests/feature_coverage_p_e2e.rs::when_comma_conditions_and_mixed_is_in`,
  `::when_widened_subject_boxes_every_primitive_comparand`.
- **`private` visibility is LEXICAL, and the JVM's is not.** A nested (non-`inner`) class, the
  companion and an `inline` body spliced into a caller all sit inside the owner's braces, so Kotlin
  lets them reach its private members; each is a SEPARATE class file, so `invokespecial` on a private
  method and `getfield`/`putfield` on a private backing field are both illegal there. Accessibility is
  therefore decided over the ENCLOSING chain (not the receiver chain, which a nested class has none
  of), and the reach is realized through the synthetic bridges kotlinc emits on the owner —
  `access$<name>` for a method, `access$get<X>$p` / `access$set<X>$p` for a property. Both are applied
  at the single point the call/read/write node is CONSTRUCTED, so no lowering path can forget them; a
  call with an omitted (defaulted) argument is left alone, since the bridge carries no `$default`
  stub. This removed the divergence where a class with a companion kept public accessors for its
  private properties. Tests: `tests/companion_e2e.rs::companion_reaches_the_outer_class_private_var`,
  `::a_nested_class_reaches_the_outer_class_private_member`,
  `::a_private_member_of_an_unrelated_class_stays_inaccessible`,
  `::property_inferred_from_generic_companion_method`, box `classes/kt504.kt`.
- **The accessor a `private` property does not get is the SYNTHESIZED one.** A source-written
  accessor is user code with a body: skipping it replaces the program's `set(l) { /* ignore */ }` with
  a plain field store, so the write silently takes effect. Only the synthesized `getX`/`setX` pair is
  withheld. Test: `tests/companion_e2e.rs::a_private_property_keeps_its_source_written_setter`,
  box `properties/kt3551.kt`.
- **A property reference carries its type arguments.** `::foo` typed as a RAW `KProperty0`, so
  `(::foo).get()` erased to the upper bound and `(::foo).get().value` did not resolve. The reference
  type is built with the property's own type (`[V]` at arity 0, `[Recv, V]` at arity 1). Two things
  are deliberately NOT asserted, because a wrong type is worse than none: a type still mentioning a
  type parameter (the use site's substitution is not applied here), and an EXTENSION property's value
  type (written in terms of the property's own parameters). A VALUE-CLASS-typed property reference
  declines outright — kotlinc emits those accessors under the value-class name mangle, which the
  reference does not yet carry. Tests: `tests/toplevel_property_ref_e2e.rs::toplevel_property_refs_run`,
  box `callableReference/property/extensionPropertyWithExtensionType.kt`,
  `inlineClasses/callableReferences/inlineClassTypeMemberVar.kt`.
- **A property on a BUILTIN receiver is one table, read by both phases.** `String.length`, `Char.code`
  and an array's `size` have no class file to resolve against. The body checker knew them; the
  SIGNATURE phase did not, so `const val code = a.code` reported "cannot infer the type of property"
  for an expression the checker accepts. `String.length` alone records its resolved member — the other
  two are backend intrinsics, and recording a member for them retargets the read into unverifiable
  bytecode. Test: `tests/toplevel_property_inference_e2e.rs::toplevel_property_cross_reference`.
- **A lambda may carry its own label, and a labelled return is LOCAL to it.** `run rr@{ … }` puts the
  label tokens between the callee and the `{`, which ended the postfix parse before the block: the
  lambda was never attached as an argument and the callee reported as an unresolved reference. Every
  site that decides whether a labelled return is local now asks for the lambda's EFFECTIVE label — its
  own when written, else the name of the function it is passed to. `return@run v` itself lowered as
  the ENCLOSING function's return, pushing the lambda's value where the function's type is required
  (a `VerifyError`, not merely a wrong answer); it now breaks out of a splice frame, the same
  mechanism a user `inline fun` already used. A body whose every path is a labelled return still
  declines: the checker types the call from the `Nothing` fall-through, so there is no result type to
  bind — typing a lambda from the JOIN of its labelled returns is the checker-side fix that shape
  needs. Tests: `tests/inline_vc_suspend_coverage_e2e.rs::labelled_trailing_lambda_parses`,
  `::labelled_return_leaves_the_lambda_not_the_function`,
  `::inline_local_labeled_return`.
- **A lambda argument to the invoke operator is CONTEXTUAL.** `b { it + 1 }` on a
  `class Box { operator fun invoke(f: (Int) -> Int) }` types `it` from the operator's parameter. The
  arguments were typed with no expectation, so `it` came out as the erased upper bound and the call
  reported "operator cannot be applied to 'Any' and 'Int'" before the operator was ever consulted —
  the expectation has to be supplied when the arguments are typed, not after selection. The same
  lambda on a normally-named method (`b.run2 { it + 1 }`) always bound correctly, so this is specific
  to the operator-invoke call shape. A FUNCTION-VALUE receiver supplies its own parameters through the
  identical convention, and the arbitrary-callee shape (`make(n)({ … })`) takes the same seeding. Only
  the arity-free lookup is available, since this necessarily runs BEFORE any argument type exists to
  select an overload with; a receiver with no invoke convention, or an arity mismatch, falls back to
  plain argument typing unchanged. Tests:
  `tests/inline_vc_suspend_coverage_e2e.rs::inline_operator_fun`,
  `tests/invoke_operator_lambda_arg_e2e.rs`.
- **The `sequence {}` / `iterator {}` gate asks who `yield` belongs to.** Those builders drive a
  suspend lambda through `yield`/`yieldAll` suspension points, a state machine the pass does not model,
  so the file is skipped. The gate matched the SPELLING, so an ordinary user method
  (`class Buildee<T> { fun yield(arg: T) }`) skipped its file for no reason. It now gates on the
  resolved owner being `kotlin.sequences.SequenceScope` — or on the call being unresolved, where
  nothing rules the builder out. Tests:
  `tests/scope_function_value_arg_e2e.rs::apply_accepts_receiver_function_value_argument`,
  `tests/lower_bail_reason_e2e.rs::gated_corpus_cases_report_precise_lower_bail`.
- **A typealias keeps its target's type ARGUMENTS.** The parser recorded only the target's head name
  and skipped the rest of the line, so `typealias IntList = List<Int>` aliased a RAW `List` and
  `for (x in xs)` handed back the erased bound ("operator cannot be applied to 'Int' and 'Any'"). An
  alias whose target carries type arguments now expands STRUCTURALLY, through the same pass and
  use-site substitution the function-type aliases already used (`typealias Table<V> = Map<String, V>`
  → `Table<Int>` is `Map<String, Int>`). A bare `typealias A = Foo` keeps the name map, which the
  constructor-alias registration and classifier lookups are keyed by. Tests:
  `tests/feature_coverage_r_e2e.rs::typealias_in_signatures_and_bodies`,
  `tests/feature_coverage_x_e2e.rs::typealias_function_and_generic`.
- **Sealed exhaustiveness descends the hierarchy.** A sealed subclass that is ITSELF sealed is
  covered when all of ITS subclasses are: the hierarchy is a tree and only its LEAVES can be
  instantiated. Checking only the DIRECT subclasses reported
  `sealed class Node { sealed class Leaf : Node(); … }` covered by `IntLeaf`/`StrLeaf`/`Branch` as
  non-exhaustive, demanding an `is Leaf` branch kotlinc rejects as redundant. A subclass the arms DO
  cover (`is Leaf ->`) stands for its whole branch and is not re-reported through its children. The
  same tree is walked when deciding which arms COVER something: an `object` arm may name a subclass of
  a nested sealed class, so membership is tested against every sealed descendant rather than the direct
  subclasses alone. Tests: `tests/feature_coverage_r_e2e.rs::nested_sealed_hierarchy`,
  `resolve::tests::nested_sealed_hierarchy_is_exhausted_by_its_leaves`,
  `::covering_a_nested_sealed_class_directly_covers_its_branch`,
  `::a_missing_nested_sealed_leaf_is_reported_by_name`.
- **A labelled lambda splices a CALL to its impl method.** `return@<own label>` is lowered as the
  closure method's own return, so splicing the raw body would turn it into a non-local return of the
  enclosing function, carrying the wrong type. Withholding the splice form instead is not an option:
  an `@InlineOnly` callee (`sumOf`, `require`) has no callable body, so a declined splice fails the
  whole file. Splicing a call keeps the labelled return inside the impl, where it is correct — the
  same device the anonymous-function bare-return case already used. Test:
  `tests/feature_coverage_t_e2e.rs::labeled_return_from_nested_lambda`.
- **A type parameter carries every bound, not just the first.** Kotlin's `where T : A, T : B` is an
  INTERSECTION, so a member declared on ANY bound is available; `Ty::TyParam` holds a single bound —
  the erasure, which is the first, matching kotlinc's JVM rule. A member reached only through a later
  bound (`x.name` on `T : Comparable<T>, T : Named`) resolved against `Comparable` and reported as
  unresolved. The remaining bounds are kept beside the erasure and retried when the lookup fails; the
  erasure itself is untouched, so descriptors still match kotlinc. Test:
  `tests/feature_coverage_n_e2e.rs::where_clause_two_bounds`.
- **A member called on an OBJECT or COMPANION receiver types its lambda arguments from the selected
  candidate, exactly as an instance receiver does.** `Wrap.apply2 { it * 2 }` on
  `object Wrap { fun apply2(f: (Int) -> Int): Int }` must bind `it` to `Int`. The instance-receiver
  arm of `check_call` postpones lambda arguments (`None` in the partial argument types), selects the
  member against the non-lambda arguments, and only then checks each lambda against that member's
  function-type parameter (`best_module_member_candidate` → `plan_generic_member` →
  `module_member_lambda_shape` → `check_lambda_with_types`). The classifier-receiver arms reached
  `check_module_member_call` with argument types computed up front by `arg_tys`, so a lambda was
  checked with no expectation, `it` bound as `Any`, and the body was rejected
  (`operator cannot be applied to 'Any' and 'Int'`) — a lambda argument to an object member was
  effectively unusable. Those arms now share one seam, `classifier_member_arg_tys`, which runs the
  instance path's postpone-select-check sequence against the classifier's own type: the object's
  internal name for `object` members, and `C$Companion` for companion members (the receiver type
  `check_source_companion_call` already dispatches on, so selection and checking agree). It applies
  to the receiverless, receiver-lambda (`Int.() -> Int`), and defaulted/named/trailing call shapes,
  because the shape comes from the same `CallSig` slot mapping; with no lambda argument it is
  `arg_tys` unchanged. A companion is not a `this` receiver unless it declares a supertype, so an
  unqualified call to a sibling companion function from inside the companion had no implicit
  receiver carrying the member either; `implicit_member_receiver_types` adds the `C$Companion` type
  to the implicit-receiver list the member-shape lookup walks. An unqualified companion call from an
  ordinary INSTANCE member of the class stays unresolved — that is a separate scope gap (it fails
  with no lambda involved), not a lambda-typing one. Type-parameter inference for a lambda
  parameter bound by a FUNCTION-level type parameter (`fun <T> pick(v: T, f: (T) -> String)`) is
  equally absent on instance receivers and is likewise out of scope here.
  Test: `tests/object_receiver_lambda_e2e.rs`.

- **`Type { … }` selects a SOURCE companion's `operator fun invoke` when no constructor is
  applicable.** For `class Wrap(val v: Int) { companion object { operator fun invoke(f: (Int) -> Int): Int } }`,
  kotlinc resolves `Wrap { it * 2 }` to the companion operator — a lambda is not applicable to the
  constructor's `Int` parameter — while `Wrap(7)` stays a construction. krusty had this for CLASSPATH
  types (`semantic_companion_ty` + `record_invoke`) and for source INTERFACES (which have no
  constructor), but a source CLASS went to the constructor unconditionally and reported
  `return type mismatch: expected 'Int', actual 'Wrap'`. The source class path now falls back to
  `check_source_companion_call(CALLABLE_INVOKE_OPERATOR, require_operator = true)` when
  `select_source_constructor` finds no applicable candidate, lowering as
  `getstatic Wrap.Companion; invokevirtual Wrap$Companion.invoke` — kotlinc's
  `Wrap.Companion.invoke(…)`. Constructor selection still wins whenever a constructor is applicable,
  so the operator never shadows a construction. The arguments are re-typed against the operator's
  parameters and the constructor pass's diagnostics for them are dropped: that pass had no
  expectation for a lambda argument, and its complaints never applied to the call kotlinc selects.
  Selection does NOT depend on whether the argument bodies type-check — backing out of the operator
  because a lambda body has an unrelated error reported the construction's own failure
  (`cannot create an instance of an interface`) on top of that error, so the operator is taken
  whenever the call resolves to it and its own diagnostics are kept. `Ty::Error` with nothing
  reported is not a resolution: `check_module_member_call` suppresses its inapplicable-overload
  diagnostic when the call already carries an argument diagnostic, and that is precisely the
  provisional pass this would then erase, which would leave the call silent.
  Two gaps are shared with the pre-existing member-call paths and are NOT introduced here, but this
  fallback makes the first reachable from `Type { … }`: a lambda's inferred RETURN type is not
  checked against the expected function type (`O.apply2 { it + 1; "s" }` on an object receiver and
  `P().apply2 { … }` on an instance receiver are accepted identically, and fail at runtime with a
  `ClassCastException`), and an overload set whose members differ only in a POSTPONED lambda slot
  scores every candidate equally, so declaration order decides
  (`fun ap(f: (Int) -> Int)` + `fun ap(s: String)` fails on object and instance receivers alike).
  Test: `tests/object_receiver_lambda_e2e.rs`.

- **Explicit type arguments determine a call's type wherever argument mapping cannot.** Four
  behaviors around a generic provider call with defaults on both sides of a non-final vararg and a
  trailing receiver-lambda default:
  (1) the SIGNATURE phase's lightweight property inferer maps arguments positionally, so a named
  argument out of its declared position found no candidate and a class property initialized by
  such a call reported "cannot infer the type of property" — when every top-level overload agrees
  on the return after substituting the explicit type arguments (`explicit_targ_return_agreement`),
  that IS the property's type, and argument legality stays with the full checker; (2) the
  top-level lambda-shape probe (`lambda_shape_for_overload`) unified bindings only from
  receiver/arguments, so a `T.() -> Unit` trailing lambda bound its implicit `this` to `T`'s
  BOUND (`kotlin/Any`) instead of the written `<C>` — explicit type arguments now seed the
  bindings first (`seed_explicit_type_args`; `unify_ty`'s `or_insert` keeps them authoritative);
  (3) a classpath MEMBER call with explicit type arguments (`m.any<Org>()`) dropped them entirely
  (`resolve_instance_member` had no `type_args` input), returning the formal's bound;
  (4) `Type(args) { … }` where `Type`'s classpath companion declares `operator fun invoke` shapes
  the trailing lambda from the invoke parameter exactly as a top-level overload would
  (`companion_invoke_lambda_shape`), and a constructor whose mapping fails must DECLINE, not
  diagnose, when such an invoke exists; otherwise a public constructor can claim the call with a
  missing-parameter diagnostic before the semantic companion candidate is considered.
  Tests: `tests/classpath_reified_named_default_vararg_e2e.rs`,
  `tests/classpath_companion_invoke_lambda_e2e.rs`,
  `tests/classpath_member_overload_no_names_e2e.rs`.

- **A generic argument with nothing to bind its type parameter takes the enclosing member's
  parameter type — expected-type inference.** A `fun <T : Any> provide(): T` call has no argument,
  no explicit type argument, and no assignment context, so its first-pass type collapses to the
  erased bound and the enclosing member call reported "none of the following candidates is
  applicable". At the member-call LAST RESORT (every other path declined), such arguments
  (`expected_retypable_generic_argument`: a bare-name or qualified generic call whose type is the
  erased top) are re-typed against the parameter every mappable member overload agrees on, the
  bound type is recorded as the argument's type, and member resolution runs once more
  (`retry_member_call_with_expected_arguments`). Divergent overload sets decline — this pass
  cannot know which parameter the argument lands in.
  Test: `tests/classpath_member_overload_no_names_e2e.rs`.

- **An expected result fixes a call's type arguments where the declared return mentions them
  invariantly.** Value arguments contribute a LOWER bound only: `fun <T> reply(body: T): Reply<T>`
  called as `reply("s")` infers `T = String`. An invariant occurrence of `T` in the declared return
  admits exactly one solution, so a `Reply<Any>` return position forces `T = Any` — previously the
  argument-derived binding always won and the call was reported as
  `return type mismatch: expected 'Reply<Any>', actual 'Reply<String>'`. The expected type is
  related through the declared return's applied supertypes, so a Java factory declared
  `static <T> MutableReply<T> ok(T body)` is seen as `Reply<T>` where `Reply<Any>` is expected (the
  shape that made a Micronaut-style controller returning `HttpResponse<Any>` unusable, including
  through the merged branches of an `if`/`try` used as the function body). Covariant occurrences
  keep the narrower argument solution (`listOf("x")` stays `List<String>` where `List<Any>` is
  expected), a projected expectation (`Reply<out Any>`) is a bound rather than an equality, and a
  widening the argument itself cannot satisfy, or that would break a declared bound, is left alone
  so the real mismatch is still reported. Tests:
  `tests/expected_return_invariant_binding_e2e.rs`, `symbol_resolver` variance unit regressions.

- **A value never has a projected type; a projected binding is approximated.** Matching a member
  against a star-projected receiver binds the member's own formal to the PROJECTION — the stdlib
  `fun <K, V> Map<out K, V>.get(key: K): V?` applied to `Map<*, *>` binds `V` to `out Any?`. The
  call's value takes the approximation of that capture: `out X` reads as `X`, and an `in`-projected
  one reads as the FORMAL's own declared bound — `Holder<in String>` whose parameter is `T :
  CharSequence` reads back a `CharSequence`, because the projection only says a caller may write a
  `String` there. So `m["k"]` is `Any?`, exactly as kotlinc types it. Only the value's own type is
  approximated: a returned `List<out X>` is a legal type and stays as declared. ONE primitive
  decides what a projected binding means, and the SLOT's position is its only input — never the
  callee: a read sees the projection's readable bound, a write admits `Nothing`, and a
  classifier-argument position keeps the projection, because `List<out X>` is a legal type. Raw
  substitution IS that invariant rule, so it stays correct wherever a receiver or classifier argument
  is formed, while every slot that types a VALUE — parameter, return, lambda input — instantiates
  through the position-aware primitive. This is what lets `MutableList<*>.add("x")` and its extension
  spelling `MutableList<*>.setFirst("x")` both stay prohibited while `List<*>.indexOf("x")` is
  accepted: `MutableList` is invariant so its argument keeps the projection, and `List` is declared
  `out E`, which makes the matching use-site projection redundant — `List<*>` simply is `List<Any?>`.
  A formal's FIRST lower constraint likewise keeps the projection, the stand-in for kotlinc's
  captured type, so a projected argument stays applicable to the parameter it inferred. Tests:
  `tests/star_projection_member_read_e2e.rs`.

- **What a declared classifier publishes as its JVM class `Signature` is decided once, by the
  recorded signature — never by the declaration's kind.** Class, data class, object, interface,
  enum, and enum-entry subclass all obtain their writer from one place, which takes the signature
  `ir_lower` recorded for a generic declaration or parameterized supertype. The checked class model
  records an enum's implicit `Enum<E>` self argument even when the enum has no entries, so
  `enum class E : I<String>` publishes `Ljava/lang/Enum<LE;>;LI<Ljava/lang/String;>;`; a hand-rolled
  string beside the writer is what previously erased the arguments of every interface an enum
  implements. Emitting interfaces through a writer of their own is why `interface Iface<B>`
  carried no class `Signature` while `class Klass<B>` did, so every consumer read the interface as a
  raw type. The value interns between the class and superclass names, as ASM visits them.
  Test: `tests/classifier_class_signature_e2e.rs`.

- **A Java array slot is `Array<(out) T!>!`.** Java arrays are covariant, so a `Sub[]` value reaches
  a `Base[]` parameter — `setRecipients(type, InternetAddress.parse(to))` passes an
  `InternetAddress[]` to an `Address[]`, which krusty reported as an unresolved reference because no
  candidate accepted the argument. The projection belongs to the flexible, Java-sourced spelling
  alone: Kotlin's own `Array<T>` stays invariant, and `Array<Sub>` is still rejected for an
  `Array<Base>` parameter, since a store through it would be unchecked. Test:
  `tests/java_array_covariance_e2e.rs`.

- **A type parameter is a lexical binding, declared on the rung of the declaration that introduces
  it.** `class C<T>` binds `T` on its CLASS rung, `fun <T> f()` on the function's own rung — one
  namespace (`Ns::Classifier`), different declaring rung — so a parameter retires with its
  declaration instead of being replaced wholesale on a scope shared with siblings. The rung KIND
  says which it is: a declaration's signature rung is `ScopeKind::Function` (carrying no receiver),
  never the `Block` kind reserved for `if`/`when` branches, loop bodies and lambdas. `reified` is a
  field of that binding rather than a parallel set, so the two cannot drift and an
  `inline fun <reified T>` cannot leave `T` reified for the next declaration that reuses the name
  (kotlinc: `cannot use 'T' as reified type parameter. Use a class instead.`).
  The lookup walk stops at a class rung that does not carry its outer instance — the same cut
  `implicit_receivers` makes. Verified against kotlinc 2.4.10:
  `class A<T> { class B { fun g(): T? } }` is `unresolved reference 'T'`, while an `inner class`,
  a local class inside a member, and an anonymous object all still resolve `T`.
  Tests: `tests/scope_chain_e2e.rs`
  (`a_nested_class_cannot_name_the_outer_classs_type_parameter`,
  `an_inner_class_can_name_the_outer_classs_type_parameter`,
  `a_type_parameter_does_not_leak_to_the_next_declaration`,
  `a_reified_mark_does_not_leak_to_the_next_declaration`), `src/resolve/scope.rs` unit tests.

- **A local class is checked in the scope it was written in, and captures an enclosing VALUE only
  through a constructor parameter it does not have yet.** The class is hoisted to a top-level
  `Decl::Class` for signature collection and lowering, but the checker enters it from its
  `Stmt::LocalClass` (`File::local_class_decls` links the two), on a class rung with
  `carries_outer == true` — a local class captures the enclosing instance, so the enclosing
  receivers and type parameters stay reachable. Verified against kotlinc 2.4.10:
  `class A<T> { fun m() { class L { fun k(): T? = null } } }` compiles, as does a local class whose
  own property shadows a same-named member of the enclosing class. Signature collection sees the
  hoisted declaration without that context, so the enclosing declaration's type parameters are
  supplied to it explicitly (`local_class_enclosing_tparams`).
  A local class's hoisted declaration is named after the declaration it was written in
  (`Outer.m.Local` → `Outer$m$Local`), which is both how kotlinc names one and the spelling every
  lexical-prefix walk in the compiler already understands; nothing in the source is rewritten to
  match, because the SOURCE name is bound in `Ns::Classifier` and resolves where it was written.
  A local class is NOT a member class: its `InnerClasses` entry carries `outer_class_info_index = 0`
  and it gets an `EnclosingMethod` attribute (class only — the JVM spec permits `method_index = 0`,
  and a wrong descriptor would make `Class.getEnclosingMethod()` throw). The class that CONSTRUCTS
  it must carry the same `InnerClasses` entry, including the file facade: reflection cross-checks
  the two sides and throws `IncompatibleClassChangeError` when only one has it.
  A class literal on a local class is rejected (the file skips): reflection reports `simpleName`
  from the Kotlin `@Metadata` local-class marking, which krusty does not emit, so the name would come
  back qualified (`codegen/box/reflection/classes/localClassSimpleName.kt`).

  WHAT a local class captures is decided in that scope — the only place the enclosing bindings
  exist — and recorded as `TypeInfo::local_class_captures_by_class`. How a capture is represented is
  lowering's decision: each captured binding becomes a leading constructor parameter and field, and
  `Lower::emit_new` supplies them ahead of the source arguments at every construction, so no
  argument-mapping arm can forget them. `ClassSig::ctor_params` stays the SOURCE signature and is
  indexed by source position — captures are not in it.
  The enclosing INSTANCE is the second capture kind — the receiver itself rather than a binding in
  the chain — and is carried as ONE capture however many of its members are read, placed FIRST
  because lowering identifies it by position (field 0), which is what an outer member read and a
  `this@Outer` both go through. It is rejected when the enclosing receiver is a `@JvmInline value
  class`: there is no instance to capture, since `this` is the bare underlying value there
  (`codegen/box/inlineClasses/initBlock.kt` fails verification otherwise).
  Three capture shapes are NOT modelled yet and are rejected (the file skips): a local function
  (which carries captures of its own), a reassigned `var` (shared mutable state, not capturable by
  value), and a capture read during CONSTRUCTION — an initializer, an `init` block, a
  base-constructor argument, a secondary constructor, or a primary-constructor parameter default. The capture scan is syntactic and
  conservative: over-reporting skips a file, under-reporting emits a class without what it needs,
  which the box corpus caught as `NoSuchMethodError` on construction
  (`codegen/box/localClasses/capturingInDefaultConstructorParameter.kt`).
  Tests: `tests/local_class_scope_e2e.rs`.

- **Fully-qualified name references resolve by SEGMENT ITERATION over a package/classifier
  namespace, not by matching spellings.** Kotlin admits a fully-qualified reference with no import
  wherever a simple name is legal — `pkg.Cls()`, `pkg.Cls.COMPANION_VALUE`, `pkg.Obj.fn()`,
  `val x: pkg.Cls`, `pkg.Cls::class`, `a is pkg.Cls`, `pkg.topLevelFun()`,
  `java.util.concurrent.atomic.AtomicInteger(1)`. There is no syntax that separates the package part
  from the classifier part: `a.b.C.D` is ambiguous between package `a.b` + class `C` + member `D`,
  package `a` + class `b.C` + …, and so on. So a dotted reference is resolved one segment at a time,
  and every prefix has one committed meaning — a **value**, **package**, or **classifier**
  (`ResolvedQualifier`, `src/resolve.rs`). Under a package, the next segment is a classifier of
  that package or a subpackage; under a classifier, it is a nested classifier. A missing edge ends
  with a typed `QualifierError`; resolution never flattens the spelling or retries alternative `$`
  placements. The owning position resolves exactly one terminal edge from the committed prefix.
  This is what makes the qualified spelling end at the SAME resolved identity the imported simple
  name reaches, for a same-module source classifier and a classpath one alike — the walk consults the
  federated module + classpath source at every step, so origin never enters the rule.

  Source package declarations are namespace facts of their own; they do not disappear when every
  declaration in a package is retained only for conflict diagnostics. Java stub overlays likewise
  contribute their containing packages. Signature bootstrap presents source declaration identities
  and libraries through the same qualifier interface, so it does not need a second source-path walk.

  Resolving a prefix to a **package** requires a package namespace, which is the half that was
  missing: `SymbolSource::package_exists` (`src/symbol_source.rs`), answered by the classpath's
  package catalog (`PackageTree::has_package` — jars, class directories, and the JDK jimage, plus the
  intermediate packages that own no classes of their own, so `java` answers as well as `java/util`)
  and by the module's own declarations (`ModuleSymbols::package_exists`, derived from the package of
  every declared classifier and facade). Packages UNION across sources rather than shadowing: the
  same package legitimately holds module and classpath declarations.

  Two shadowing rules, both taken from the reference compiler:
  - **A value root shadows both a classifier and a package.** kotlinc resolves the leftmost segment
    as an expression first, so a local, a member property reached through an implicit receiver, a
    top-level property, or a property brought in by an EXPLICIT OR WILDCARD import makes the
    reference a member chain. With `import other.plib` (or `import other.*`) binding a property named
    `plib`, `plib.Cls` reads `plib`'s `Cls` member and does not name the class `plib.Cls`.
  - **An in-scope classifier shadows a package path**, so `Outer.Nested` resolves through the
    in-scope chain and never through a package named `Outer`.

  Package references are ABSOLUTE from the root: inside `package top`, `sub.Deep()` does NOT resolve
  to `top.sub.Deep` (kotlinc: `unresolved reference 'sub'`), unlike Java.

  Covered in both origins: construction (including a nested classifier and one under an `object`),
  companion/static const/val/var read and write, companion function call, `object` member read/write
  and call, an `object` or companion reached as a VALUE, nested-`object` members, enum entries, type
  annotations (nullable, type arguments, explicit type arguments, supertypes), `is`/`as`/`when is`,
  class literals (including `pkg.Cls.Nested::class` and `java.util.ArrayList::class`), and
  package-qualified top-level function, property, `const val`, and `var` write; enum synthetic statics
  (`values()`/`valueOf`); an explicitly named `Companion`; an unbound callable reference
  (`pkg.Cls::method`); and construction through a `typealias`. A `typealias` resolves to its TARGET on
  both sides of the pipeline — the module's alias edges are keyed by fully-qualified name
  (`SymbolTable::source_alias_fqns`, since a per-file key cannot answer a reference from another
  file), and lowering follows the same edge, because returning the alias spelling made the lowered
  internal disagree with the checker's recorded result type and the construction was dropped.

  Alias edges are exposed by `SymbolSource::resolve_type_name`, so they participate in the same walk;
  there is no alias-table fallback after a segment fails.

  Lowering consumes the identity the checker resolved and never re-derives it from source spelling:
  type references use `TypeInfo::resolved_type_ref`, constructors use `resolved_constructor`, and a
  `const val` read is a facade FIELD (it has no accessor — calling one threw `NoSuchMethodError`).
  A callable declared in another SOURCE file of the module carries no physical descriptor, so a
  receiver-less static call to one is emitted as `Callee::CrossFile`, which derives the descriptor
  from the signature; emitting the library form wrote an EMPTY descriptor, which the JVM rejects as a
  zero-length constant-pool entry.
  Tests: `tests/qualified_name_e2e.rs`.

- **An unresolvable callee is UNRESOLVED_REFERENCE, not a call-specific diagnostic.** kotlinc has no
  "unresolved function" diagnostic: when the callee of `f(...)` names nothing at all — no function,
  no constructor, no classifier, no value — the frontend reports its ordinary
  `Unresolved reference '{0}'.`, the same text a bare unresolved name gets. krusty reports it
  lowercase-first (`unresolved reference 'f'.`) and the LSP boundary sentence-cases it, so both the
  CLI and the language server agree with the reference frontend.
  Tests: `tests/diagnostics_match_kotlinc.rs`.

- **FUNCTION_EXPECTED requires a selected non-callable expression.** A member call such as
  `holder.count()` first selects `Holder.count: Int`; because that value carries no `invoke`, kotlinc
  reports `Expression 'count' of type 'Int' cannot be invoked as a function. Function 'invoke()' is
  not found.` Bare `count()` has different call-tower semantics: a non-callable local, parameter, or
  property contributes no callable candidate, so an otherwise missing callable is `unresolved
  reference 'count'.` and a same-named classifier constructor may still win (`val Registry = 1;
  Registry()` constructs the class). These decisions come from semantic value/callable shapes, never
  from a spelling-derived function guess or an error-type fallback.
  Tests: `tests/function_expected_e2e.rs`.

- **A `typealias` spelled in a DECLARED type survives into `@Metadata`.** A type alias is
  transparent to every semantic question — `Cargo` and `Payload` are the same type, assignable and
  comparable without conversion — but Kotlin still records which of the two source WROTE. A declared
  type (parameter, return, property, receiver, supertype, type-parameter bound, constructor
  parameter) that names an alias emits the expanded classifier as `Type.class_name` and the spelling
  as `Type.abbreviated_type` (field 13, whose `Type.type_alias_name` is field 12). This is per TYPE
  NODE and recursive: `List<Cargo>` abbreviates the argument, not the `List`. Only the OUTERMOST
  alias of a chain is recorded. An alias in CODE position (`Cargo(7)`) is not a declared type and
  carries none, and an `import x.Y as Z` rename is not a typealias and carries none either.
  Consequently the alias identity is surface syntax, never semantics: it is carried BESIDE `Ty` (see
  `crate::spelling`) precisely so that no type comparison, interner bucket, or hash lookup can split
  on it. Byte-identity against kotlinc is the definition of correct here, not decode equivalence —
  the spelling changes the `d2` string table, so a merely "equivalent" encoding is observably
  different. See `docs/METADATA_NOTES.md` for the wire rules and interning order.
  Tests: `tests/typealias_abbreviated_type_e2e.rs`.

- **A qualified `typealias` spelling denotes its TARGET, not the alias.** `app.Cargo` and `Cargo`
  name the same declaration and must resolve identically. A dotted spelling reaches name resolution
  intact — the parse seam expands only what it can match — and qualified resolution answers it with
  the alias's own declaration, because an alias declaration IS a name its package contains. That is
  correct for resolving the NAME and wrong for the TYPE it denotes: an alias is a resolution edge,
  never a classifier. Resolving it as one made the alias its own type, and the emitted descriptor
  named `app/Cargo` — a class nothing declares or emits, so the class file would fail to load. The
  two alias kinds must also agree: a function-type alias has no classifier at all, so the same
  treatment could only report `unresolved reference`, rejecting valid Kotlin. Both are expanded by
  matching the alias's own qualified spelling, so neither depends on the alias having a target class.
  Tests: `tests/typealias_abbreviated_type_e2e.rs`.

- **A context parameter precedes the extension receiver in the JVM signature.** Kotlin signs a
  context extension `(contexts…, receiver, values…)`: `context(c: String) fun Src.plain(x: Int)` is
  `(Ljava/lang/String;Lrepro/Src;I)Ljava/lang/String;`, and with two contexts
  `context(c: String, d: Int) fun Src.two(x: Int)` is `(Ljava/lang/String;ILrepro/Src;I)…`. The
  receiver's index is therefore the CONTEXT COUNT, not zero — `params[0]` is the receiver only for an
  extension that declares no `context(…)` clause. A context function with no receiver is unaffected,
  which is why the pure top-level form was already correct. A CLASS-BODY extension signs the same way
  — its dispatch receiver is `this`, and among the method parameters the context prefix still precedes
  the extension receiver — but it builds its physical list in a different place, so correcting only
  the top-level path leaves a half-fixed ABI. krusty modelled the reverse,
  `(receiver, contexts…, values…)`, and did so symmetrically on both sides of the boundary: it
  emitted that order and decoded classpath descriptors expecting it. Nothing inside a single krusty
  compilation could disagree, so every same-module test passed while every context extension was
  ABI-incompatible with kotlinc in both directions — reading one back from a kotlinc-built dependency
  took its first context parameter for the receiver, matched no candidate, and fell through to "no
  supported semantic lowering". This is the layout krusty already used for a context FUNCTION TYPE,
  whose receiver sits at `params[context_count]`, so declarations and function types now agree.
  The semantic (receiver-free) parameter list keeps the context prefix and is consequently not a
  contiguous slice of the physical one, hence `Cow`. Note the metadata `d1`/`d2` string tables still
  intern the receiver before the context parameter where kotlinc interns the reverse; the records are
  keyed by protobuf field number, so both compilers read either encoding, and kotlinc resolves a
  krusty-built context extension. Whole-facade byte identity additionally awaits unrelated gaps
  (string-concatenation lowering, `SourceDebugExtension`, `ACC_VARARGS`).

  Two consequences follow for the RECORD rather than the descriptor. A context function keeps its
  `JvmMethodSignature` handle, as kotlinc's does: the receiver's slot is not recoverable from the
  proto alone, so a reader without the handle derives `(receiver, contexts…, values…)` and targets a
  method nothing declares — a call that links and then fails at run time. And a CLASS member records
  its context parameters as `Function.context_parameter` (field 13); published as ordinary value
  parameters they are demanded positionally, and kotlinc rejects the call with
  "no value passed for parameter". `$default` mask bits are numbered over the SOURCE parameters —
  context prefix INCLUDED, extension receiver excluded — which is how kotlinc's own stub reads them.
  Tests: `tests/context_parameter_signature_order_e2e.rs`.

- **A context argument is an inference source and an ordinary boxing site.** The value selected for a
  context parameter constrains the declaration's type variables like any other argument: in
  `context(c: T) fun <T> Src.tagged(x: String): T`, `with(42) { Src().tagged("a") }` has type `Int`.
  Symmetrically, the context prefix must not consume the arguments that follow it — in
  `context(c: String) fun <T> Src.valued(x: T): T` the WRITTEN argument pins `T`, so zipping a
  context-inclusive parameter list against the call's arguments shifts every binding by one and
  leaves `T` open (its members then read as `unresolved reference`). A context parameter typed by a
  type variable erases to a reference slot, so a primitive context value boxes on the way in exactly
  like a written argument; omitting that left an `int` in an `Object` slot, which is a `VerifyError`
  at class load rather than a wrong answer.
  Tests: `tests/context_parameter_signature_order_e2e.rs`.

## 8. Success criteria for the PoC

1. krusty compiles the `kotlin-memory-bench` `many_functions` / `multifile` / `bodyheavy` programs.
2. **ABI match:** public members (names/descriptors/modifiers) are identical to kotlinc's output.
3. **`@Metadata` match:** emitted metadata decodes to the same Kotlin declarations as kotlinc
   (compatible `metadataVersion`), so output is consumable as a Kotlin library — verified by having
   kotlinc itself compile a consumer against krusty's output.
4. **Behavior match:** execution-differential tests pass on the §7 edge cases.
5. Measured peak RSS compiling `bodyheavy` is **bounded ~constant in file count** and well below
   kotlinc's (the per-file thesis, on a real implementation).
6. All emitted classes pass the JVM verifier.

> Note: criteria 2–3 are the load-bearing compatibility goals; byte-identity is explicitly out.
> The ultimate compat test (criterion 3) is **round-trip**: compile a library with krusty, then
> compile a *Kotlin consumer* of it with real kotlinc — if kotlinc accepts krusty's `@Metadata` and
> resolves the API, the output is a genuine Kotlin library.

- **Local functions** (`fun` inside a function body): a non-capturing local function is lifted to a
  `private static` method on the facade, mangled `$local$<stmtId>` (the checker assigns the name and
  rejects captures). Calls route through the checker's `local_call_map` to the lifted `FunId`
  (`Callee::Local`). Recursion and multiple local functions in one body work. A local function that
  captures an enclosing variable, or is generic, is still skipped.

- **Capturing local functions**: a local function that captures enclosing locals is lifted with those
  captures prepended as extra leading parameters (then its declared parameters). A captured `val` (or a
  `var` the function writes — boxed into a shared `kotlin/jvm/internal/Ref$XxxRef`) is supported: the
  written `var`'s holder is passed so the mutation is visible to the enclosing scope. A captured `var`
  the function only *reads* is rejected (it could be reassigned in the enclosing scope after the call,
  making the by-value capture stale) — the checker records `local_fun_captures` as ordered `(name,
  type)` and the lowerer passes each captured value (or holder) at the call site.

- **Anonymous-object capture** (`object : I { … }`): captured parameters, read-only locals, and
  initialized immutable enclosing properties become synthetic constructor properties. Property
  initializers and delegates see constructor properties and earlier backing properties, not later ones.

- **Captured-`var` boxing rule** (precise): a captured `var` is boxed into a `Ref$XxxRef` iff it is
  *reassigned somewhere in the function* (`fn_reassigned`, scanned over the whole body including nested
  closures). A captured `var` that's never reassigned is effectively final and passed by value, like a
  `val` — matching kotlinc and avoiding needless boxing. This covers a `var` a closure only reads but
  the enclosing scope reassigns after the closure is built (KT-4656). Unsigned `UInt`/`ULong` share the
  signed `Ref$IntRef`/`Ref$LongRef` holder (their unboxed JVM representation).

- **Inner-class outer access**: an inner method reads an enclosing-instance member through `this$0`
  (field 0) via the outer's synthesized getter (`this.this$0.getX()`) — the outer backing field is
  private, so direct field access would be illegal. The checker makes the outer class's backing-field
  properties resolvable as implicit-`this` members of the inner class (in both signature collection,
  for return-type inference, and body checking). An inner property initializer may combine outer and
  own members (`val z = x + y`); the constructor body scopes `this$0` as the first parameter value.

- **Nullable primitives** (`Int?`/`Long?`/`Char?`/…): modeled as their boxed JVM wrapper
  (`Int?` = `java/lang/Integer`) everywhere — `resolve_ty`, `ir_lower::ty_of`, and the `Stmt::Local`
  slot type all map a nullable primitive to its wrapper (so a boxed value is never stored in a
  primitive slot). A primitive is assignable to its wrapper (boxed at the emit site:
  `Integer.valueOf`); `x!!` narrows a wrapper to its unboxed primitive (the checker types it as the
  primitive, the lowerer unboxes after the null check). Unsigned/value-type nullables stay unsupported
  (skipped). Also fixed a generic vararg with a primitive type argument (`mk<Long>(-1, …)`): each
  element is coerced to the type-argument primitive before boxing, so `-1` becomes a `Long`, not an
  `Integer`.

- **Nullable-primitive equality + generic literal coercion**: `nullablePrimitive == primitive` (`a == 5`)
  is allowed — the primitive operand is boxed for structural equality (`Intrinsics.areEqual`). Float/Double
  are excluded (their `0.0 == -0.0` IEEE-754 semantics differ between primitive `==` and boxed `equals`).
  A generic constructor with a primitive type argument (`Box<Long>(-1)`) coerces each non-nullable
  type-parameter field's literal to the type-argument primitive before boxing (so `-1` becomes `Long`,
  not `Integer`). An assignment to a typed `var` coerces a generic-erased `Object` value to the slot
  type (the `checkcast` kotlinc inserts) so the slot's stackmap frame stays consistent.

- **Nullable-primitive equality short-circuits the primitive side** (matches kotlinc): `wrapper == prim`
  (and `!=`) lowers to `{ val t = wrapper; if (t == null) <fixed> else t.unbox <op> prim }`, where the
  fixed null-result is `false` for `==` / `true` for `!=`. The primitive operand is evaluated **only** in
  the non-null branch, so a side-effecting RHS (`a?.x != sideEffecting()`) runs exactly when kotlinc runs
  it — once when the wrapper is non-null, never when it is null. (A general `Any == prim`, where the
  reference side is *not* a nullable-primitive wrapper, still boxes the primitive for `Intrinsics.areEqual`.)

- **Safe calls on classpath receivers** (`s?.length`, `list?.size`, `s?.substring(1)`): the `?.` member
  is resolved against the classpath — a user method/field, else a library member via `resolve_instance`
  (args lowered to their parameter types) — not just same-module targets. A safe call whose member returns
  a primitive (`String?.length` → `Int`) types as the boxed wrapper (`Int?`) and boxes the primitive result
  before the `null` join, so the `when` arms agree; the checker maps such a result back through
  `nullable_prim_wrapper` so the expression's type is the wrapper, not `Error`.

- **Extension-function body referencing receiver members implicitly** (`fun A.twice() = n + n`, where
  `n` means `this.n`): the bare name lowers as a read on the receiver — which is bound as the `this`
  local with `cur_class == None` (an extension is a top-level static, not a class member). Because the
  body executes *outside* class `A`, a user property is read through its getter (the backing field is
  private), falling back to a direct field then a classpath accessor; this mirrors any external member
  read. **Nullable reference receivers** (`fun A?.foo()`) are now supported for *ordinary* names: under
  `Ty`'s nullability erasure a lone `A?.foo` is unambiguous (there is no member `foo` to compete with).
  An *operator*-named extension on a nullable receiver (`fun String?.plus(…)`) stays rejected: it would
  shadow the builtin/member operator for *every* `String + …` (even non-null), recursing infinitely in a
  body that uses the same operator — kotlinc disambiguates by static nullability, which krusty cannot.
  A duplicate or nullable/non-null pair with the same erased `(receiver, name)` is also rejected.

- **Diagnostic wording tracks kotlinc 2.4.0** (a drop-in replacement should print the same errors). An
  unresolved name reads `unresolved reference 'q'.` (quoted, trailing period); a reassigned `val` reads
  `'val' cannot be reassigned.`; a return-position type error (an expression/getter body) reads
  `return type mismatch: expected 'String', actual 'Int'.`; explicit initializers, assignments,
  arguments, and Boolean conditions use kotlinc's distinct `initializer type mismatch`,
  `assignment type mismatch`, `argument type mismatch`, and `condition type mismatch` forms. Missing
  callable arguments name the first absent parameter; excess function/member/constructor arguments
  render the source signature (including generic and context parameters); and an overloaded or
  otherwise inapplicable candidate set starts with `none of the following candidates is applicable:`.
  An unknown named argument reads `no parameter with name 'unknown' found.` and points at the
  argument name; the LSP publishes that exact name range with the official sentence-cased message.
  A repeated named argument reads `argument already passed for this parameter.` and points at the
  repeated label rather than the first occurrence. Invalid reordered mixing reads
  `mixing named and positional arguments is not allowed unless the order of the arguments matches the
  order of the parameters.` and points at the positional argument expression. Missing required parameters
  are then reported in declaration order, excluding defaulted and vararg parameters. A trailing lambda
  cannot supply a final vararg and reads
  `passing value as a vararg is allowed only inside a parenthesized argument list.`; normal overload
  selection still takes precedence.
- **A `vararg` parameter is always omittable in default-argument resolution.** Kotlin metadata never
  sets `declares_default_value` on a `vararg` — it is implicitly omittable — so
  `CallSig::has_known_required_param` skips the vararg slot (mirroring the call-arg slot mapper).
  Without this, a classpath function with BOTH a defaulted parameter and a `vararg`
  (`fun f(a: Int = 0, vararg xs: T)`, called as `f()`) rejected its own `$default`
  candidate on a call omitting both, reporting `unresolved function 'f'`. The emit side matches:
  a top-level `$default` callable carries the vararg slot/element to the lowerer (as the extension
  path already did), the shape-based element-pack branch yields to the `default_call` branch, and an
  omitted vararg lowers as an EMPTY array with NO mask bit — kotlinc's `$default` passes the array
  straight through, so a null placeholder trips the callee's non-null vararg check
  (`classpath_default_vararg_call_e2e`, including a JVM box run). Known gap: the named-array form
  `f(more = arrayOf(x))` with an omitted default before the vararg still fails to map.
  A NAMED argument that also omits a default (`foo(y = "Y")` skipping `x`) maps through the
  checker's recorded argument→slot mapping at every `$default` emit site — the bare-name path once
  ignored it and bound the argument to the FIRST slot while masking the LAST, silently swapping
  the parameters (`named_args_classpath_e2e::named_arg_omitting_a_default_maps_to_its_own_slot`).
  Calling an ordinary member or a concrete non-null extension through a nullable receiver reports
  `only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver of type 'T?'.`
  at the unsafe `.`. A nullable-receiver extension remains callable through ordinary dot syntax and
  may shadow a same-named member on the non-null type; safe calls, `!!`, and smart-cast receivers remain
  valid. Extension applicability retains source and Kotlin-metadata receiver nullability rather than
  deriving it from the erased JVM signature; `<T>` has the nullable `Any?` upper bound while
  `<T : Any>` rejects nullable values. The LSP highlights the unsafe dot, including when comments
  separate it from the member name. Explicit, same-package, star, and default imports retain Kotlin
  precedence, and equally applicable star-imported extensions remain ambiguous.
  Unresolved member reads and calls use the same `unresolved reference` form as bare names. Verified
  by the differential `diagnostics_match_kotlinc` tests, which compile the snippets with both
  compilers, report all mismatches in one run, cover cross-file generic signatures, and assert the
  first `error:` text matches exactly. LSP diagnostics identify their source as `Kotlin`, matching
  the official Kotlin server. The official server sentence-cases those messages for display; the
  LSP boundary uppercases the first ASCII byte in place while the compiler-owned CLI message remains
  byte-for-byte compatible with kotlinc.

- **Semantic highlighting follows the official Kotlin LSP symbol model.** Data classes highlight as
  `struct`; ordinary classes, enums, interfaces, annotations, and objects use `class`, `enum`,
  `interface`, `decorator`, and `type`. Kotlin properties remain `property` even at top level;
  every primary-constructor declaration is a readonly `parameter`, including `val`/`var` property
  parameters, while references to those property parameters are `property` and retain their actual
  mutability. Top-level callables carry `static`; source enum entries are readonly `enumMember`s
  without `static`; immutable values carry `readonly`; mutable values carry `modification`; suspend
  functions carry `async`; abstract declarations carry `abstract`; deprecated declarations carry
  `deprecated`; operator functions use `operator`; Kotlin builtins and resolved `kotlin.*` library
  symbols carry `defaultLibrary`. Every declaration also carries `declaration`. References select
  the narrowest enclosing lexical binding, and range responses include tokens intersecting either
  boundary. Qualified references use the checked receiver class, so same-named members on different
  classes retain distinct categories and mutability modifiers. Source-only flags (`data`, `operator`,
  and source deprecation) are shared across every file in the analyzed source set.
  (`semantic_tokens_match_official_kotlin_symbol_classification`,
  `semantic_tokens_match_official_constructor_and_enum_modifiers`,
  `semantic_tokens_respect_lexical_shadowing_between_functions`,
  `semantic_tokens_resolve_qualified_members_and_deprecated_references`,
  `semantic_tokens_preserve_source_set_metadata_across_files`,
  `initialize_and_requests_expose_full_and_range_semantic_highlighting`.)

- **Completion uses compiler-derived source-set and lexical symbols.** The server returns an LSP
  `CompletionList`, advertises item resolution, and maps Kotlin declarations to the official
  completion kinds (`method`, `operator`, `function`, `property`, `constant`, `variable`, `struct`,
  `interface`, `enum`, `enumMember`, and `typeParameter`). Unqualified completion selects only declarations whose
  lexical scope and declaration position contain the cursor, with the narrowest same-name binding
  winning. Source top-level declarations are shared across same-package open files (or files that
  explicitly import them without an alias); unrelated unimported packages do not leak unusable
  entries. A simple
  source-defined receiver followed by `.` or `?.` completes its own and inherited accessible
  properties and methods, companion members, or enum entries even when parser recovery prevents
  type checking that edit. Completion and
  `completionItem/resolve` read compact cached snapshots and never rerun compiler analysis.
  This increment catalogs public/internal source declarations; private/protected completion remains
  a later context-aware access-control increment and is not advertised by the current snapshot.
  (`completion_survives_an_incomplete_safe_member_access`,
  `completion_snapshot_interns_strings_into_compact_array_entries`,
  `completion_includes_inherited_members`,
  `completion_does_not_offer_unimported_cross_package_symbols`,
  `completion_is_scoped_compiler_backed_and_resolvable`,
  `completion_includes_cross_file_top_level_declarations`.)

- **Go-to-definition returns official Kotlin LSP source locations.** The server advertises
  `definitionProvider` and returns the declaration-name range as an array of LSP `Location`s. Current
  source navigation covers class/type references, function parameters, constructor and body
  properties, lexical values/functions, same-package/imported top-level declarations, and the exact
  checker-selected top-level overload across open files. Query handling never reruns analysis.
  Long-lived state is an integer-only 20-byte entry `(source lo, source hi, target file, target lo,
  target hi)`, globally capped at 256K entries per source set; compiler ASTs, source copies, and symbol
  strings are dropped with the worker analysis. The opt-in official differential compares exact target
  URI and UTF-16 start/end positions for same-file, cross-file, lexical, member, class, and overload
  cases. (`definition_matches_official_class_parameter_and_property_ranges`,
  `definition_resolves_an_exact_cross_file_function_location`,
  `definition_prefers_local_values_and_functions`,
  `definition_uses_the_checker_selected_overload`,
  `definition_keeps_same_named_classes_package_qualified`,
  `definition_snapshot_uses_compact_file_and_span_entries`.)

- **Go-to-type-definition returns checked source-class locations.** The server advertises
  `typeDefinitionProvider` and reduces explicit type references, parameter/local declarations,
  inferred and nullable values, ordinary explicit/inferred property declarations, constructor
  results, property reads, and source class declarations to exact source class-name ranges. The
  query reads a 20-byte integer-only index and never reruns analysis. Definition and type-definition
  share a 256K-entry source-set navigation cap: definitions are built first and type-definition
  consumes the remainder, so this feature cannot enlarge the prior worst-case navigation worker
  frame. The worker drops ASTs, checked type tables, and its temporary `TypeName` target map after
  emitting the snapshot. No type name or source string is retained in the index. The official
  differential compares complete location values and exact UTF-16 endpoints, including ordinary
  properties and a query after a supplementary-plane character.
  (`type_definition_snapshot_is_compact_source_free_and_exact`,
  `type_definition_resolves_exact_cross_file_utf16_location_without_reanalysis`,
  `shared_navigation_budget_keeps_saturated_worker_response_below_frame_cap`.)
- **Go-to-implementation matches transitive Kotlin source implementations.** The server advertises
  `implementationProvider` and returns exact class-name or member-name locations for every transitive
  source subclass/implementor or overriding function/property. Declaration, supertype-reference,
  selected member-call, and property-read queries resolve through the same compiler-selected
  declaration identity. Checked signatures and arity shortlist candidates; parser-owned type
  patterns then preserve nullability and class/method parameter identity while substitutions follow
  direct inheritance edges across non-declaring intermediate classes. Only proven declaration edges
  are closed transitively, so an unrelated descendant overload cannot attach to a generic
  grandparent. A constant-factor work budget bounds hierarchy traversal and structural comparisons.
  Ambiguous same-arity fallbacks are omitted instead of returning a wrong overload. Parser-owned
  constructor-property modality excludes same-named non-overrides and private/final parents without
  rescanning source.
  The worker reduces results to the same 20-byte integer-only entries as definition and drops ASTs,
  method references, names, and hierarchy catalogs. Definition entries consume first from the
  256K navigation-entry cap shared with type-definition and implementation, so the feature cannot
  enlarge the prior worst-case worker frame. Requests use cached spans,
  never rerun analysis, return `null` when no implementation exists, and clear stale indexes on
  incomplete or source-limit-blocked refreshes. The official differential compares complete sorted
  URI/range arrays and exact UTF-16 endpoints for transitive classes, generic methods, overloads,
  and queries following a supplementary-plane character. Focused tests cover properties,
  constructor-property modifiers, private/final exclusions, and bounded traversal/storage.
  (`implementation_snapshot_is_compact_transitive_generic_and_overload_exact`,
  `constructor_property_override_uses_the_exact_declaration_span`,
  `private_constructor_property_is_not_implemented_by_a_same_named_child_property`,
  `semantic_navigation_occurrences_share_the_construction_limit`,
  `ancestor_walk_is_iterative_cycle_safe_and_work_bounded`,
  `implementation_resolves_exact_transitive_cross_file_utf16_locations_without_reanalysis`,
  `implementation_locations_match_official_kotlin_lsp_exactly`,
  `shared_navigation_budget_keeps_saturated_worker_response_below_frame_cap`.)

- **Find-references reuses exact navigation identities.** The server advertises
  `referencesProvider` and returns source `Location`s whose compact definition target matches the
  symbol under the cursor. `includeDeclaration` includes or removes only the declaration's own
  identifier range. Cross-file functions, lexical values, classes, overloads, and imports therefore
  preserve the same symbol disambiguation as go-to-definition. The query deduplicates cursor targets
  in request-local memory and performs one bounded scan of the existing globally capped 20-byte
  definition entries, so it retains no reverse-index copy, compiler AST, source copy, or symbol
  string and never reruns compiler analysis.
  (`references_match_exact_cross_file_ranges_and_declaration_filtering`,
  `definition_snapshot_reverse_query_reuses_the_same_compact_entries`.)

- **Rename reconstructs exact official edits from compact navigation spans.** The server advertises
  `renameProvider` and uses the same checker-selected definition identities as definition and
  references, preserving lexical, overload, and cross-file disambiguation. It reads each occurrence
  spelling from the authoritative open-document string only while handling the request and emits
  the official server's minimal, ordered `documentChanges`, with exact document versions and UTF-16
  start/end positions. Identifier diff work, distinct transient spellings, and estimated expanded
  response bytes are capped. The compiler AST and long-lived LSP snapshots retain only file/span
  identities—never copied source text or rename strings. The official differential compares the
  complete response for cross-file, local, selected-overload, Unicode-offset, and backticked cases.
  (`rename_matches_official_minimal_edits_exactly_without_reanalysis`,
  `rename_bounds_identifier_diff_work_and_expanded_output`.)

- **Incremental document synchronization preserves compiler isolation and exact locations.** The
  server advertises LSP incremental sync, applies every notification's UTF-16 ranged edits in order
  to its single retained open-document `String`, and defers a burst to one compiler analysis.
  Invalid multi-edit notifications roll back with request-local replaced fragments and do not advance
  the document version; edit count, cumulative UTF-16 scanning, text mutation, and retained rollback
  fragments are bounded per notification. A source-limit-blocked document rejects ranged edits until
  a full replacement restores synchronization. No AST or compiler-front-end node retains LSP source
  text. The official LSP differential applies the same edits to both servers and compares the
  resulting definition URI and both UTF-16 range endpoints.

- **Document symbols match the official hierarchical Kotlin model and exact locations.** The server
  advertises `documentSymbolProvider` and returns ordered nested declarations for top-level
  functions/properties/classes/type aliases, primary and secondary constructors, constructor
  properties, members, nested classes, enum entries, and companion objects. Official kinds include
  `Struct` for data classes and `Object` for companions; deprecated declarations carry both the
  legacy flag and `Deprecated` tag. Local declarations are omitted, matching the official server.
  Every full and selection range is converted to UTF-16 once in the compiler worker. Long-lived state
  is a bounded 40-byte packed record plus interned names—never an AST, source slice, or second source
  string—and requests only encode that cached hierarchy. The opt-in official differential compares
  the complete response, including hierarchy, kinds, tags, and every range endpoint.

- **Signature help matches official source-call labels, overload selection, and parameter ranges.**
  The server advertises the official trigger/retrigger characters and handles top-level overloads,
  constructors, members, local functions, generic call-site substitutions, default and vararg
  parameters, named-argument reordering, Unicode names, and nested calls. Each parameter label range
  is an exact UTF-16 pair, including the official named-argument cursor behavior. Long-lived state is
  bounded to 32-byte call records, 12-byte signature/parameter records, 8-byte argument records, and
  interned strings; it retains neither compiler AST nodes nor another source string. Containment links
  plus sorted argument endpoints make the cached-index lookup
  `O(log calls + nesting depth + log arguments)` after the request position's linear UTF-16-to-byte
  conversion, and requests never rerun compiler analysis. Named/generic overload customization is materialized for one
  call at a time, charged to the source-set wire budget immediately, serialized, and dropped; the
  bounded declaration catalog is never cloned across all call sites. Discovery sorts only bounded
  12-byte `(ExprId, span)` call sites; argument shapes and names are then derived one call at a time,
  with name bytes included in the same wire budget. Generic substitution recurses
  through nullable and nested class arguments. The opt-in
  differential compares the complete response for source declarations against Kotlin LSP 262.8190.0.
  Classpath signature documentation remains dependent on a future source-attachment/KDoc metadata
  provider; it is not fabricated from callable names.

- **Hover returns official Kotlin LSP signatures and locations.** The server returns fenced Kotlin
  markdown for source symbols and the exact UTF-16 identifier range, and returns `null` for literals
  where the official server does. Signatures include inferred and nullable types, receiver types,
  generic bounds, modality, visibility, and selected overload parameters. Requests use a cached
  12-byte `(source lo, source hi, interned signature id)` entry and never rerun analysis. Signature
  strings are deduplicated and bounded; compiler ASTs and source-text copies are dropped after the
  worker builds the snapshot. The opt-in official differential compares the entire hover result,
  including markdown and both range endpoints.

- **A property reference is a function value** (`C::n` as a `(C)->Int`). An unbound `Type::prop` has type
  `KProperty1<C, R>` and a bound `obj::prop` has `KProperty0<R>`; both are accepted where a `(C)->R` /
  `()->R` (`kotlin/jvm/functions/Function1`/`Function0`) of the matching arity is expected, because
  kotlinc's `PropertyReference{1,0}Impl` implements the corresponding `FunctionN` (`invoke = get`). This
  assignability holds in three places: the checker's `expect_assignable` (a declared function-typed
  local/parameter), the JVM library overload resolution (`arg_fits` — so `Iterable.map(C::n)` selects the
  `Function1` overload), and the IR lowering of a function-typed local (`val f: (C)->Int = C::n` records
  the slot's type from the *annotation*'s `Ty::Fun`, not the initializer's `KProperty1`, so a later
  `f(arg)` lowers through the `Function1.invoke` path). The reference lowers to the existing
  `PropertyReference{1,0}Impl` singleton/instance — no new IR. (Arity is read structurally from the
  `FunctionN`/`KPropertyN` class name, never by member-name matching.)

- **Integer-family `rangeTo` widening + generic-vararg literal adaptation.** A range expression `a..b`
  (as a *value*) follows kotlinc's `rangeTo` overloads: `Char..Char` is a `CharRange`; any combination of
  `Byte`/`Short`/`Int` yields an `IntRange`, and a `Long` operand makes a `LongRange` (the bounds are
  coerced to the element type — `Byte`→`Int` is a no-op on the JVM stack). Iterating a stored range value
  uses the same overflow-safe counted loop as a direct `for` (break when the counter reaches the inclusive
  `last` *before* incrementing, so a range ending at `Int.MAX_VALUE`/`Long.MAX_VALUE` doesn't wrap past it
  and spin). Separately, a generic `vararg` resolved with a bound element type (`listOf<Long>(3, 4)`)
  adapts integer **literals** to that element type — the literal `3` is the constant `3L`, boxed as `Long`,
  not `Integer` — matching kotlinc's compile-time literal adaptation. Only constant literals adapt (a
  non-literal `Int` in that position is a kotlinc error, so krusty never silently inserts an `i2l`). The
  bound element type is carried on `LibraryCallable.vararg_elem`, recovered from the callee's generic
  signature with the call's explicit type arguments bound first. (Direct `for (x in b1..b5)` over `Byte`/
  `Short` via the `Stmt::For` path is still pending — only range *values* widen so far.)

- **Direct `for` over a `Byte`/`Short` range + step type coercion.** A direct `for (x in b1..b5)` over
  `Byte`/`Short` operands (the `Stmt::For` path, distinct from a range *value*) widens to an `IntRange`:
  the counter is `Int` and the bounds coerce up (`Short.rangeTo(Short): IntRange`). The loop `step` is
  coerced to the counter's type — `for (i in 0L..n step 3)` adapts the `Int` step `3` to `Long`, else an
  `int` would be stored into a `long` slot (a verify error). Both mirror the range-value path (phase 369).

- **Operator overloading via a library function + most-specific overload selection.** A binary operator
  on a reference receiver desugars to its operator function (`a + b` → `a.plus(b)`, `-`→`minus`, `*`→
  `times`, `/`→`div`, `%`→`rem`) resolved through the library set — so `list + element` →
  `CollectionsKt.plus`. Resolving this required fixing extension-overload selection generally: the
  candidate filter is now subtype-aware (`arg_fits_subtype`, so a `List` argument matches an `Iterable`
  parameter), and among all fitting candidates the **most specific** is chosen — the one whose non-receiver
  parameters are each a subtype of every other candidate's. Without this, `list + list` would bind the
  erased-`Object` element overload (`plus(Iterable<T>, T)`) and nest the list instead of selecting the
  concat overload (`plus(Iterable<T>, Iterable<T>)`). The lowering re-resolves and emits the call
  (`inline` per the callee). Incomparable candidates fall back to first-match (stable).

- **Unsigned `in`-range membership + a fast test profile.** `x in a..b` / `x !in a..b` for `UInt`/`ULong`
  operands lowers to the same bounds-check intrinsic as the signed case, but each comparison goes through
  `Integer.compareUnsigned`/`Long.compareUnsigned` (`compareUnsigned(p, q) <op> 0`) rather than a signed
  opcode — so values past the sign bit (`4000000000u`) order correctly, matching kotlinc's `uintCompare`.
  Iterating an unsigned range *value* (`for (i in 0u..n)`, which needs the mangled `UIntRange` getters) is
  still pending; direct `for (i in 0u until n)` already worked. (Infra: the in-loop test round now builds
  with an unoptimized `gate` cargo profile — overflow-checks off so krusty's wrapping arithmetic doesn't
  abort — for seconds-long rebuilds; the conformance worker stack is 64 MB so unoptimized recursion fits.)

- **Unsigned range *values* + inline-class mangled-member resolution.** `0u..5u` / `0uL..nuL` builds a
  `UIntRange`/`ULongRange` (the public ctor takes a trailing synthetic `DefaultConstructorMarker`, passed
  `null`), and iterating one (`val r = 0u..5u; for (i in r)`) reads its bounds through kotlinc's MANGLED
  inline-class getters (`getFirst-pVg5ArA`/`getLast-…`, inherited from the `…Progression` superclass). The
  mangle suffix is a hash of the inline-class signature; rather than recompute it, krusty looks the real
  JVM name up from the classpath by prefix (new `LibrarySet::mangled_member`, walking the superclass
  chain). The counted loop compares with `Integer/Long.compareUnsigned` so values past the signed sign bit
  iterate in unsigned order, and breaks at `i == last` before incrementing (overflow-safe). This is the
  first piece of real inline-class infrastructure (the mangled-name lookup); unsigned open-ranges/`step`
  are still unmodeled, so most unsigned-range corpus files stay skipped — but the range-value iteration
  itself is correct (verified past the sign bit). (`UByte`/`UShort` were unmodeled at that pass; they are
  first-class `Ty` variants now — see the unsigned-types entry above.)

- **`if`/`when` branch join: primitive with `null` → boxed nullable wrapper.** When one branch of an
  `if`/`when` expression is a primitive and another is `null` (`if (c) true else null`), the result type is
  the primitive's boxed nullable wrapper (`Boolean?` = `java/lang/Boolean`), matching kotlinc. For this to
  verify, the branch lowering now coerces each branch to the result type when that type is a reference —
  the primitive branch is boxed at the merge so all branches agree on the (reference) stack type. (A
  broader "two unrelated references → `Any`" join was tried and reverted: it unblocked files whose merge
  frame krusty's emitter couldn't reconcile — a VerifyError — so reference↔reference joins beyond `null`
  stay unsupported pending correct common-supertype frame merging.)

- **`super.method(args)` — non-virtual base dispatch.** A `super` method call compiles to `invokespecial`
  on `this` (value 0) targeting the named base method, skipping the receiver's own override. The base is
  the current class's direct superclass; the signature is resolved from a user base (via `method_of`) or a
  classpath base (`resolve_instance`, so `class C : ArrayList<…>() { … super.add(x) }` and
  `super.toString()` reaching `Object`/an open stdlib method work). Modeled by a new `Callee::Special`
  (the first non-virtual instance-call node). `owner` is the direct superclass — the JVM resolves
  `invokespecial` up the chain to the actual declaring class.

- **`if`/`when` branch join: two values of the same class.** Two branches whose static types are the
  same class (`List<C>` and `List<D>`, or `A` and `A`) join to that class with erased type arguments
  (`List<*>`). The runtime class is identical, so the merge stack frame is exactly that class — safe to
  emit (unlike a join of *unrelated* references, which would merge to `Object`, a frame krusty's emitter
  can't yet reconcile; those stay unsupported). Type arguments are erased to none at the join, so a member
  read on the result resolves against the raw class (element type `Any`). The same semantic path handles
  builtin and mixed frontend/object spellings: `String` with `String?` joins to `String?`, while non-null
  `Ty::String` with non-null `Obj("kotlin/String")` remains non-null `String`. Nullability is derived from
  the original operands rather than from whether their internal representations compare equal. This join
  is representation-generic; it does not branch on whether a type came from the current file, another
  module file, or the classpath (`tests/elvis_nullability_join_e2e.rs`).

- **`if`/`when` branch join: unrelated reference classes → common supertype (`Object`).** Two branches of
  different reference classes (`if (c) Foo() else Bar()`) join to their common supertype, which krusty
  approximates as `Any`/`Object` (the universal upper bound). The emitter writes `Object` for the
  merge-point stack frame, so each branch's more-specific value verifies against it; an assignment/return
  to a more specific declared type inserts the `checkcast` kotlinc emits (the value really is that type at
  runtime). Branch types are compared by their JVM internal name when deciding whether a merge is needed —
  `Ty::String` and `Ty::Obj("java/lang/String")` are the same type but distinct `Ty` values, so a
  same-class merge keeps its precise frame and only a genuinely different class falls back to `Object`.

- **Property getter bridges (covariant / generic-erased overrides).** A property that overrides a
  supertype property with a different erased type — a covariant `override val from: NodeImpl` over
  `val from: Node`, or a generic interface `val x: T` (erased to `Object`) overridden with a concrete
  type — gets a synthetic `ACC_BRIDGE` getter `getX()` returning the *supertype's* (erased) type that
  delegates (`invokevirtual`) to the concrete `getX()`. Without it, a read through the supertype reference
  resolves to the absent erased getter (an `AbstractMethodError`). The concrete getter's return is a
  subtype of the bridge's, so no cast is needed. Synthesized in the lowering (reusing the method-bridge
  emit); a primitive own type (which would need (un)boxing in the getter bridge) is still rejected.

- **Bridges with a primitive concrete type.** A getter or method bridge whose concrete member returns a
  primitive (a generic `val x: T`/`fun f(): T` erased to `Object` overridden with `: Int`, or a covariant
  primitive-backed return) is now synthesized: the `ACC_BRIDGE` boxes the primitive return to the erased
  reference type (`Integer` for an `Object` bridge). The bridge emitter already performed this boxing —
  the checker/lowering were over-conservatively rejecting the case, so the guards were removed.

- **`as` to a primitive type (unbox cast).** `x as Int` on a reference operand compiles to `checkcast
  Integer; intValue()` — the `ImplicitCoercion` reference→primitive path the emitter already provides
  (`unbox_to`: checkcast the wrapper, then the value method). A wrong dynamic type throws
  `ClassCastException` at the `checkcast`, matching kotlinc. Each standard primitive is supported; `UInt`/
  `ULong` are excluded (their cast needs the inline-class box, not `Integer`). A nullable primitive target
  (`x as Int?`) resolves to the boxed wrapper and is unaffected.

- **`ByteArray`/`ShortArray`/`FloatArray` constructors + data-class array-property skip.** The checker's
  primitive-array-element table (`Ty::primitive_array_element`) was missing `ByteArray`/`ShortArray`/
  `FloatArray` though the lowering always handled all eight, so `ByteArray(n)` etc. were "unresolved" —
  added the three. Separately, a `data class` with an array property is now skipped: krusty erases the
  array field to an `Object` field and synthesizes `equals`/`hashCode`/`toString` with reference semantics
  rather than kotlinc's `Arrays.equals`/`hashCode`/`toString`, so it would miscompile (a property-type
  array data field is not modeled yet).

- **Data-class array properties (replaces the phase-382 skip).** `ty_of` now resolves `IntArray`/…/
  `Array<T>` to a real array type instead of erasing to `Any`, so an array field keeps its `[I`/`[Z`/…
  descriptor (not `Object`). A data class then renders an array property's `toString` with
  `java.util.Arrays.toString` (content: `[1, 2, 3]`), but its `equals`/`hashCode` keep array REFERENCE
  identity — matching kotlinc exactly: two data-class instances with equal-content but different array
  instances are NOT equal (`dataClasses/equals/intarray.kt`), while `toString` shows the content
  (`dataClasses/toString/primitiveArrays.kt`).

- **Dotted type references (`lib.Thing`, `Wrap.Box`) use the same segment walk.** Type position skips
  only the value namespace: it resolves an in-scope classifier root first, otherwise an absolute root
  package, then commits left-to-right. `Wrap` therefore shadows a package named `Wrap`; if
  `Wrap.Box` is absent, resolution fails there and never retries `Wrap/Box`. Signature collection and
  checking share the single `walk_qualifier` transition loop; lowering reads the recorded type identity.

- **Overload selection begins after qualification.** An unqualified call considers one scope-tower
  level at a time. Each import/package level supplies candidate FQNs, and the federated `SymbolSource`
  contributes every module and library overload at those identities. The first level containing an
  applicable candidate wins; an inapplicable local function therefore allows a top-level candidate,
  while an applicable local function wins without mixing priorities. The chosen semantic callable is
  recorded for lowering; overload selection never changes or retries the qualifier.

- **Fully-qualified SOURCE class names (`pkg1.Cls`) in type position.** A dotted type name whose path
  matches a class declared in the same module (a sibling file's package, no `import` needed — as
  kotlinc accepts) resolves to that source class, shadowing any classpath type of the same path. The
  import pass uses the same committed segment transitions; positions the parser stores already
  internalized (`pkg1/Cls` — supertypes, delegation specs) take the same path. No alternate JVM-name
  candidate list is generated. The same rule applies to explicit-import source paths. Resolving the
  identity does not widen access: module classifiers carry their declaring
  file, so a top-level `private` FQN remains inaccessible from a sibling file while the declaring file
  can still use it. Covers every signature-pass type position — extension receivers
  (`fun pkg1.Cls.fn()`), parameter/return/property types, type arguments, generic bounds, supertype
  lists, and typealias targets (`fq_source_typeref_e2e`).

- **Unannotated top-level computed-property getter inference.** An expression getter uses the same
  lightweight value-scope inference as a property initializer: named context parameters first, then
  module properties, so context shadowing and nested reads (`holder.value`) require no getter-specific
  name-resolution branches. After collection, unresolved computed getters retry to a bounded fixed
  point because getter bodies may legally reference later declarations; eager initializer ordering is
  unchanged. The bound is the number of pending getters, so self/mutual cycles terminate as `Error` and
  receive the normal inference diagnostic (`computed_prop_e2e`, resolve unit regression).

- **A member property's type at a use site substitutes the receiver's type arguments everywhere**
  (`SymbolTable::applied_member_prop_ty`): `Holder<A>.a` is `A`, not the erased `T`; an inherited
  declaration substitutes through the hierarchy (`Leaf : Mid<String>` binds `T = String` on `Mid.v`),
  and a nested shape substitutes recursively (`Holder<String>.cell: Cell<T>` becomes `Cell<String>`).
  Signature-time computed-getter inference uses the lookup entry point; ordinary checker reads,
  read-only probes, and stable-path validation reuse its lower-level semantic-owner operation. This
  keeps every consumer on one substitution rule — previously inference read the DECLARED (erased)
  type, so
  `class B(val holder: Holder<A>) { val a get() = holder.a }` collected `a: Any` and every member
  read on it failed. A directly stored `T?` remains conservative because specializing it to a
  nullable scalar requires an erased-reference/boxing boundary not modeled here
  (`computed_prop_generic_return_e2e`).

- **A property access retains both its logical use-site and semantic declaration types.** The
  emitter lowers per file, so a sibling source class has no classfile to ask; deriving an accessor
  descriptor from the read's substituted logical type produced `Holder.getA:()LA;` — a
  `NoSuchMethodError` against the erased `()Ljava/lang/Object;`. Common lowering now records the
  declaration type without branching on file/module/classpath origin or choosing a JVM accessor
  spelling. The JVM emitter uses that semantic type only when it must derive a declaration-less
  descriptor, then bridges the erased result to the logical type with a `checkcast`
  (`computed_prop_generic_return_e2e::cross_file_generic_member_read_uses_erased_accessor`).

- **Member computed getters retry to a bounded fixpoint.** An unannotated member expression getter
  (`val a get() = holder.a`) infers during its class's collection, so a referenced class collected
  later — another file, or a later class in this one — left the property typed `Error` (and the
  lowerer bailed). Like the top-level fixpoint, pending getters retry after the class walk with
  their first-pass scope, its `Error` entries refreshed from the live signatures each round so
  chains through preceding siblings converge (bounded; cycles stay `Error`), in either file order
  (`computed_prop_generic_return_e2e`).

- **Zero-arg construction of an all-default classpath value class (`Id()`).** A `@JvmInline value class
  Id(val v: String = "x")` has no synthetic no-arg `<init>` (unlike a plain all-default class); kotlinc
  constructs `Id()` via the static `constructor-impl$default(dummy, mask, marker)`, which fills the
  default itself. krusty resolves the 0-arg call only when the underlying is a REFERENCE and the classpath
  exposes `constructor-impl$default` (`value_class_ctor_has_default`), and lowers it to
  `constructor-impl$default(null, 1, null)` (single param ⇒ mask 1). A scalar underlying (its dummy slot
  can't take `null`) stays a sound skip (`classpath_type_ref_e2e`).

- **Comparison operators on a classpath `Comparable` type (`a < b`).** `<`/`<=`/`>`/`>=` on a classpath
  type whose `compareTo(o): Int` is a classpath member (not user IR) desugar to `a.compareTo(b) < 0`,
  resolved via the library set. Guarded to a REFERENCE right operand: an erased generic
  `Comparable<Double>.compareTo(Object)` with a primitive argument would need a box this path doesn't
  apply, so it stays a sound skip (`classpath_type_ref_e2e`).

- **Multi-line `catch` parameter.** `catch (\n e: Exception\n)` now parses — the parser skips newlines
  around the catch parameter exactly as an ordinary parameter list allows (`multiline_catch_e2e`).

- **Exhaustive `when` over a CLASSPATH `sealed` class with no `else`.** A `when (d) { is D.A -> …; is
  D.B -> … }` over a classpath `sealed` `D` is exhaustive (hence an EXPRESSION) when every direct
  subtype is covered — the same rule as a same-module sealed subject, but the subtype set now comes from
  the classpath `@Metadata` (`Class.sealedSubclassFqName`, proto field 16, decoded by
  `class_sealed_subclasses` behind `SymbolSource::sealed_subclasses`). `when_sealed_exhaustive` reads
  those subclasses when the subject class isn't a same-module sealed, so an exhaustive classpath `when`
  used as a value type-checks (a non-exhaustive one still errors). (`build702_gg1_sealed_when_e2e`)

- **Exhaustive `when` over a CLASSPATH enum (Java or Kotlin).** A `when (c) { p.Color.RED -> …;
  p.Color.GREEN -> … }` over an enum resolved from the classpath is exhaustive when every constant is
  covered, including when constants use fully qualified names. Missing constants are still diagnosed.
  (`when_classpath_java_enum_e2e`)

- **`suspend` `$default` member call feeding an `if`/`when` CONDITION.** A suspension in an `if`/`when`
  CONDITION (rather than a bound `val`) is hoisted to a preceding bound temp by the coroutine pass — in
  a `return if (…)`, a lambda's tail `if`-expression, and a `val a = if (…)` init — so the state-machine
  builder never meets a condition-suspending `When` it can't model. A `suspend` member with a defaulted
  parameter (`suspend fun list(f: Filt = Filt())`) is called through the `$default` synthetic, whose
  descriptor ERASES the return to `Object`; the hoisted temp now carries the member's LOGICAL return
  (recorded in `suspend_calls` from `fi.callable.ret`, not the `$default` descriptor return), so
  `bind_from_r` unboxes it and a following `t == 5` compares int with int rather than Object with int
  (which VerifyError'd). (`build702_dd1_suspend_default_e2e`)

- **Fully-qualified library top-level call with a trailing lambda (`kotlinx.coroutines.runBlocking {
  … }`).** A FQ call to a library top-level function written without an `import`, with a SYNTACTIC
  trailing lambda whose leading parameters default, now resolves. The FQ-call path re-types the trailing
  lambda against the callee's block parameter (a receiver / suspend SAM, `CoroutineScope.() -> T`) using
  the same `top_level_lambda_param_types`/`top_level_lambda_receivers` shape data the bare-name
  (`import`ed) path uses, so overload resolution binds the block's result type-parameter (`runBlocking {
  "x" }: String`); the lowerer emits `runBlocking$default(context, block, mask, marker)`. A plain
  no-lambda FQ call (`kotlin.math.max`) was already supported. (`build702_fq_trailing_lambda_e2e`)

- **A class method's DEFAULT parameter is now type-checked.** `check_method` types each parameter's
  default expression (as `check_fun` already did for top-level functions), so a NON-literal member
  default — a constructor call `fun list(f: Filt = Filt())`, an object read — records its type. Without
  it the `$default` stub lowering couldn't recognize the construction (`info.ty` was `Error`) and bailed
  ("call Filt"); a literal default (`x: Int = 5`) was unaffected. (`build722_dd1_suspend_member_default_e2e`)

- **A classpath `object`'s `INSTANCE` read inside a suspend lambda.** The coroutine `box_returns` pass —
  which boxes the returns of a CPS body / lambda state machine — now treats an `ExternalStaticField`
  (`getstatic lib/R.INSTANCE`, reading a classpath `object`) as a leaf value, like `GetStatic`. Reading a
  classpath object inside a `runBlocking { … Service(R) … }` block previously bailed the lambda's state
  machine. (`build722_dd1_suspend_member_default_e2e`)

- **The same inline HOF spliced in both branches of an `if`/`when`.** `emit_when` tracked the operand
  stack with a linear counter; a branch that left its value on the counter (height 1) leaked that height
  into the NEXT branch, which is actually reached by a conditional JUMP at the pre-branch baseline (height
  0). A framed inline splice (e.g. `xs.find { … }`'s loop body) requires an empty operand baseline, so the
  second branch's splice bailed ("inline splice failed"). `emit_when` now resets the stack counter to the
  branch-entry height at each jump-reached branch. (`build722_hh1_inline_hof_both_branches_e2e`)

- **A class literal on a REIFIED type parameter (`T::class`).** Inside an `inline fun <reified T>`, a
  class literal `T::class` is now accepted (a non-reified `T::class` still errors, as kotlinc rejects it —
  the checker tracks the enclosing function's `reified` parameters in `reified_tparams`). It records an
  unbound class literal marked with the parameter name; the lowerer, expanding the inline body with
  `reified_subst` bound to the call's type argument, substitutes `T` to that concrete type and emits its
  class constant (`nameOf<Widget>()` → `Widget::class`). (`build722_reified_class_literal_e2e`)

- **A REIFIED classpath extension delegating to a `KClass<T>`-parameter member (build.775 ee1).** An
  `inline fun <reified T : Any> Reg.getFor(id: Aid): T = getFor(id, T::class)` (a value-class parameter
  mangles the JVM name; `Reg` is a `typealias`) called `r.getFor<Prov>(id).go()` now compiles AND runs.
  Four pieces landed: **(a)** overload selection no longer prefers the same-named 2-required-parameter
  classpath MEMBER over the 1-parameter reified EXTENSION — `best_member_overload`'s prefix (under-
  application) match is gated on `required <= args.len()`, so a 1-arg call doesn't spuriously bind a
  2-required member (which erased the generic return to `Any`, breaking `.go()`). **(b)** the extension's
  reified return `T` binds to the explicit `<Prov>` via the existing `bind_extension_callable` path (now
  reached). **(c)** the reified inline body is SPLICED from bytecode: the checker stores the resolved call
  type arguments (`TypeInfo::resolved_call_type_args`), the lowerer records `[(T, Prov)]` on the IR call
  (`IrFile::reified_call_subst`), and `jvm::inline::splice_unified` NOPs each `Intrinsics.
  reifiedOperationMarker` and repoints the following type-bearing op at the concrete type. **(d)** the
  KClass mode (`reifiedOperationMarker(…, "T"); ldc class <erased>; Reflection.getOrCreateKotlinClass`) is
  handled: the erased `ldc class` operand is repointed to `Prov` while `getOrCreateKotlinClass` is KEPT, so
  the runtime value is a real `KClass<Prov>` (repointing WITHOUT keeping `getOrCreateKotlinClass` would
  miscompile). A malformed/unhandled reified marker cleanly SKIPS the whole splice (never miscompiles).
  (`build775_ee1_reified_vc_ext_e2e`)

- **An ARITY-inapplicable same-named member does not hide an applicable EXTENSION.** For a synthetic
  `catalog.loadAll<Entry>()` example where `Catalog` declares `fun <T> loadAll(type: KClass<*>)` and
  the same module declares `inline fun <reified T : Any> Catalog.loadAll(): List<T>`, kotlinc resolves
  the extension because member precedence applies only to applicable
  candidates. The classpath-member slot-mapping path now asks the federated extension overload selector
  whether the exact call can fall through before it reports a member mapping/type error. That single
  applicability query covers same-file, sibling-module, and classpath extensions with the call's labels,
  explicit type arguments, integer-literal provenance, and lambda-literal shape; qualified calls also
  probe member extensions through their ordinary instantiated-candidate path. An inapplicable extension
  does not suppress the member diagnostic, and the implicit-receiver path declines vararg extensions it
  cannot realize.
  (`member_extension_function_e2e::arity_inapplicable_member_falls_through_to_reified_extension`,
  `…_implicit_receiver`, `…_still_errors_when_source_extension_inapplicable`,
  `…_reified_extension_run`, `…_sibling_module_extension`, `…_classpath_extension`)

- **A `suspend` call as a STATEMENT in a coroutine-builder lambda + implicit-`Unit` suspend fns
  (build.775 aa1/ii1).** `runBlocking { f(r); if (…) … }` (a bare suspend-call statement followed by more
  code) no longer skips the file — the single-suspension lambda lowering falls back to the general
  lambda-mode state machine for any block shape instead of bailing on a non-`Variable` leading statement.
  And a `suspend fun` whose body FALLS THROUGH with no explicit `return` (an implicit-`Unit` body ending in
  a suspension or a `for`/`while` loop, e.g. `suspend fun f(r, xs) { for (x in xs) r.del(x) }`) now gets a
  terminal `return Unit.INSTANCE` in its state machine — without it the final resume state ran off the end
  of the `when(label)` dispatch, fell back to the `while(true)` top, and re-dispatched the same label
  forever (an infinite loop / coroutine that never completes). A bare suspending `Block` STATEMENT (the
  `for`-loop iterator desugar) is now spliced into the state-machine flattening stream.
  (`build775_ii1_suspend_for_loop_e2e`)

- **A coroutine builder infers its result type from the block (`runBlocking { … } : T`, build.775 aa1).**
  `runBlocking<T>(block: suspend CoroutineScope.() -> T): T` — and any generic top-level fn with a
  suspend-lambda parameter — used to type its result `Any`, so a value flowing OUT into a non-suspend
  context (`val c = runBlocking { repo.byId(x) } ?: error(); c.scheduledAt`, the real hit
  `member … on Any` ×7) lost the block's type. Two erasure layers hid `T`: (1) the `$default` synthetic the
  omitted-context call resolves to carries no generic `Signature`, so binding failed and the erased
  `Object` return leaked — `resolve_top_level_default_callable` now falls back to the BASE function's gsig
  (matched by parameter shape). (2) The suspend SAM erases the result into `Function2<Recv,
  Continuation<T>, Object>` while the lambda argument erases its own `Continuation` type argument to `Any`
  and carries its real result in the `Ty::Fun` return — `unify_gsig` now binds `T` from the lambda's return
  for a trailing-`Continuation<T>` param. The lowerer then `checkcast`/unboxes the erased `Object` return
  to the substituted type (a non-inline classpath call with an `Object` physical return now coerces, like
  the inline path). Passing tests only masked this: `fun box(): String = runBlocking { … }` supplied the
  result type from `box`'s return. (`build775_aa1_suspend_iface_param_elvis_e2e`)

- **A classpath collection property with a PRIMITIVE element canonicalizes to Kotlin form (build.840).** A
  data-class property `data class Ch(val items: List<Int>)` had its type recovered from the getter's
  generic signature verbatim — `java/util/List<java/lang/Integer>` — so the collection typed as raw
  `java/util/List` (not `kotlin/collections/List`) and the element as boxed `java/lang/Integer` (not `Int`):
  `for (x in c.items) { s += x }` reported "operator cannot be applied to 'Int' and 'java/lang/Integer'",
  `c.items.sum()` was "unresolved method 'sum' on 'java/util/List'". `concrete_generic_ret` (the non-suspend
  member-return recovery) now runs its result through `canonicalize_jvm_collections`, which maps the JVM
  collection to its Kotlin form AND a boxed primitive wrapper in a type-ARGUMENT position to the Kotlin
  PRIMITIVE (`java/lang/Integer` → `Ty::Int`) — mirroring the suspend-return path. So the member/`for`/
  extension resolves on the recovered Kotlin type and the element unboxes. (Element nullability stays a
  known gap — a JVM signature doesn't encode `List<Int?>` vs `List<Int>`.)
  (`build840_collection_property_element_e2e`)

- **A safe call to a lambda-taking extension types the lambda from the non-null receiver (build.840 mm1).**
  `c?.takeIf { it.at > 0 }` (a `?.` call to `takeIf`/`takeUnless`/any lambda extension) typed the lambda
  argument naively — no expected parameter type — so `it` defaulted to `Any` and `it.at` failed with
  "unresolved member 'at' on kotlin/Any". The `Expr::SafeCall` checker arm now types a lambda argument
  against the extension's block parameter, bound by the NON-NULL receiver (`rt.non_null()`), exactly as the
  non-safe path does (`?.let`/`?.run`/`?.also` already routed through the scope-function path). Non-lambda
  arguments are typed once and reused (no double evaluation). (`build840_mm1_safe_call_lambda_ext_e2e`)

- **A function parameter may be named after a modifier soft keyword (build.840 jj1).** Kotlin's only real
  parameter modifiers are `vararg`/`noinline`/`crossinline` (+ annotations); every other modifier keyword
  (`open`, `sealed`, `abstract`, `private`, …) is a soft keyword usable as a plain identifier, so
  `fun f(open: Int)` is valid. The parser's `skip_decl_prefix` treated ANY modifier-spelled ident as a
  modifier and consumed it, then reported "expected parameter name". It now leaves a modifier ident that is
  immediately followed by `:` for the name parse (a genuine modifier never precedes a colon) — which also
  handles an annotated modifier-keyword name (`@Anno open: Int`). (`build840_jj1_param_soft_keyword_e2e`)

- **An inline HOF lambda may call an ENCLOSING-class member (build.840 kk1).** `class H { fun f(es) =
  es.find { same(it.v, 3) }; fun same(a, b) = … }` — the inline-spliced `find` lambda calls `same`, a method
  of the enclosing class. krusty cleared `cur_class` for a spliced lambda's body (only a REAL closure
  captured the enclosing `this`), so the bare member call `same(…)` failed to resolve and the file bailed
  with "this construct is not yet supported by the IR backend". `lower_lambda_sam` now captures the
  enclosing `this` for an inline-splice lambda too — the splicer remaps it (like any captured local) to the
  enclosing method's slot 0, so the member call resolves and lowers. The `this`-use scan is SHALLOW for a
  spliced lambda (a `this` used only inside a NESTED lambda is that lambda's own capture), matching the
  shallow named-capture scan. `forEach { member() }` already worked (a `for`-loop desugar).
  (`build840_kk1_inline_hof_enclosing_member_e2e`; box-OK 2379→2380)

- **A lambda using a MEMBER EXTENSION of the enclosing class through an EXPLICIT receiver captures the
  enclosing `this`.** `class Ctl { fun list(ms) = ms.map { it.toResponse() }; private fun
  Model.toResponse() = … }` — the call names only the EXTENSION receiver (`it`); its DISPATCH receiver is
  the implicit enclosing `this` the accessor call needs (`member_extension_dispatch_value`). The
  `lambda_uses_enclosing_this` scan recognized only bare-name uses (`this`, an implicit-`this` member
  access), so this dispatch use was invisible: `cur_class` was cleared for the closure body, the dispatch
  lookup found nothing, and the file bailed ("this construct is not yet supported by the IR backend").
  The scan now also consults the SELECTED targets — a `ResolvedCall::MemberExtension`, an
  `ExprLowering::MemberExtensionPropertyRead`, and a `StmtLowering::MemberExtensionPropertyWrite` whose
  owner the enclosing class is ASSIGNABLE to (assignability, not equality: the extension may be declared
  on a base class) — so member extension FUNCTION calls, PROPERTY reads and PROPERTY writes in a lambda
  all capture `this` and lower, in inline-spliced and real (invokedynamic) closures alike. An extension
  owned by an unrelated object stays uncaptured (its dispatch is the object's `INSTANCE`). The direct
  (non-lambda) call and the bare-name form (kk1 above) already worked.
  (`tests/member_extension_in_lambda_e2e.rs`.)

- **A static method DECLARED ON AN INTERFACE uses an `InterfaceMethodref` constant.** A Kotlin interface's
  `foo$default` synthetic (reached when a call OMITS an interface-declared default arg — `interface A { fun
  f(x: String = "OK") }`, `class C(val x: A) : A by x`, `C(B()).f()` → `A.f$default(...)`) is a `static`
  method ON THE INTERFACE; even an `invokestatic` to it must reference it via an `InterfaceMethodref`, else
  the JVM throws `IncompatibleClassChangeError`. The `Callee::Static` emit now picks `interface_methodref`
  when the owner is an interface (queried through the new `MethodBodies::owner_is_interface`, backed by the
  classpath's class flags); a class owner (a stdlib facade — the common case) stays `Methodref`. Surfaced by
  the ee1 overload-selection fix routing an omitted-default interface call to the correct `A.f$default`
  target. (box corpus `codegen/box/compileKotlinAgainstKotlin/delegatedDefault.kt`; box-OK 2378→2379)

- **`ifEmpty`-style TyParam-receiver extensions discriminate by the JVM descriptor's first parameter.**
  Four stdlib `ifEmpty`s reach selection as identical `Any`-receiver candidates (their `C : CharSequence`
  / `Array<out T>` receivers erase); the physical first parameter is the last discriminator — a candidate
  whose physical receiver can't hold the actual one is dropped, else the tie breaks on declaration order
  and the inliner splices the wrong overload's body (`arraylength` on a String → VerifyError).
  (`string_if_empty_selects_the_charsequence_overload`.)

- **A `suspend` function type erases to the arity+1 `FunctionN`.** `suspend () -> Unit` is a `Function1`
  at runtime (trailing `Continuation` parameter), so `as`/`is` against a suspend fn type checkcast/test
  `Function{n+1}` (KT-66093). (`suspend_fn_type_cast_targets_arity_plus_one_interface`.)

- **A suspend fn carries NO `checkNotNullParameter` on its value parameters.** kotlinc's state-machine
  RE-ENTRY call (`foo(null, continuation)`) passes null for every value parameter — the real values live
  in the continuation's spill fields — so an entry null-check would throw on resume. kotlinc emits none;
  the CPS transform now clears them. (`suspend_fn_entry_has_no_param_null_check`; unlocks the
  `WITH_COROUTINES` corpus slice.)

- **A `Unit`-returning suspend fn whose whole body is `suspendCoroutineUninterceptedOrReturn { … }`
  returns the intrinsic's value, not `Unit`.** The value IS the suspension protocol result
  (`COROUTINE_SUSPENDED` or an immediate value); returning `Unit` signals completion while the
  continuation is pending → double resume (an NPE inside `releaseIntercepted`). An EMPTY intrinsic
  block yields `Unit` explicitly. (`unit_suspend_fn_returns_intrinsic_value_not_unit`.)

- **The `// WITH_COROUTINES` helpers form an implicit `support` module in `// MODULE:` tests.** kotlinc's
  test infra compiles them as a module every declared module sees (some tests write `(support)`
  explicitly, others just `import helpers.*`) — mirrored by `krusty::conformance::inject_support_module`.

- **A non-null value class flows into its nullable form (`X` → `X?`) in ANY context.** Assignment and
  argument positions box exactly as a return does — the value-class pass inserts `box-impl` from the
  nullable target type at `SetValue`/`RefSet`/`RefNew` boundaries; the shared mutable cell of a
  captured `var x: X?` is always the `ObjectRef` (a nullable value class never holds the raw scalar).
  Generic arguments compare by class (the non-null `Obj` rule ignores them too).
  (`assignment_to_nullable_value_class_var_boxes`.)

- **A FUNCTION-type `typealias` expands structurally at the parse seam.** `typealias L = (A) -> R`
  (incl. `suspend`/`context(...)` forms) records the full target `TypeRef` in `File.type_alias_fun`;
  a post-parse pass rewrites every `TypeRef` naming the alias into the arrow form, so all downstream
  raw-`TypeRef` function-type tests (checker invoke detection, lowerer, metadata) see the ordinary
  shape. Per-file only — a sibling file's alias stays unresolved (skip, never mis-grade); generic
  function-type aliases are not expanded (use-site substitution unmodeled). The use site's `?`
  survives expansion (`L?` = nullable function type) and the span stays the use site.
  (`tests/typealias_function_type_e2e.rs`; corpus `suspendConversion/suspendConversionOfAliasedType.kt`
  advances from `unresolved` to the separate suspend-conversion gap.)

- **Suspend conversion: a NON-suspend function value flowing into a `suspend` function-type parameter
  wraps in a synthesized adapter.** kotlinc's shape: a `FunctionReferenceImpl` subclass implementing
  `Function{n+1}` plus the `kotlin/coroutines/jvm/internal/SuspendFunction` marker, whose `invoke`
  DROPS the trailing continuation and delegates to the wrapped value's erased `Function{n}.invoke` —
  a plain function never suspends, so its erased result (for `Unit`, the `Unit.INSTANCE` an erased
  Unit lambda already returns) is the completion value verbatim. The adapter class lives in the
  `$suspendConversion$` name space: its `uniq` is the arg expr id, which a callable-ref VALUE lowered
  from the same arg already claims under `$fnref$` (a shared name emits two classes under one name —
  the survivor has the wrong arity → CCE). A SUSPEND value into a suspend parameter passes through
  unchanged (both erase to `Function{n+1}`). A suspend function VALUE call in CPS position erases its
  `InvokeFunction` ret to `Object` when the continuation is threaded — a tail-forward `areturn`s the
  raw erased result (COROUTINE_SUSPENDED or the boxed value); the flattener re-applies the logical
  coercion from `ir.suspend_calls`. (`tests/suspend_conversion_e2e.rs`; corpus
  `suspendConversion/` + `callableReference/adaptedReferences/suspendConversion/` — box-OK +10.)

- **A suspend function VALUE invoked in statement position mid-body gets its own resume state.**
  The machine already threads the continuation and parks/resumes correctly; the leaf/machine
  validation walk (`box_returns`) was just missing traversal arms for `InvokeFunction` and
  `SetStatic`, so the whole file skipped. Verified with a REALLY-suspending value (parks its
  continuation, driver resumes it — completion must not happen before the resume).
  (`tests/suspend_value_invoke_e2e.rs`.) Two shapes the arms would otherwise unlock stay guarded as
  skips, never miscompiles: a suspend LAMBDA with a value-class parameter (the param spill field
  erases to the underlying but the erased `invoke` stores the boxed object — VerifyError,
  `createMangling.kt`), and a machine that combines a real suspension state with a
  `suspendCoroutineUninterceptedOrReturn` block (re-entry after the intrinsic's external resume
  misdrives the label — `suspendCoroutineFromStateMachine.kt` loops forever).

- **A field store is a value-class representation boundary, decided by the field's PRE-erasure
  declared type.** The value-class pass's boundary list covered locals (`Variable`/`SetValue`) and
  shared cells (`RefNew`/`RefSet`) but not `SetField` — so a suspend lambda's synthesized
  `invoke`/`create`, which casts each erased `Object` argument to the value class and stores it into
  the param spill field the erasure just retyped to the underlying, stored the BOXED object into an
  underlying-typed field (VerifyError). The `SetField` boundary pairs the stored value with the
  field's pre-erasure type; `Boxed → UnboxedX` then unboxes exactly like a local store. kotlinc
  parity: its erased bridge `unbox-impl`s each value-class argument before the spill (verified on
  `createMangling.kt`). NULLABLE value-class lambda parameters stay declined in
  `lower_suspend_lambda` (boxed/null spill interplay unmodeled).
  (`suspend_lambda_with_value_class_params`; corpus
  `coroutines/inlineClasses/direct/createMangling.kt` box-OK, 2921 → 2922, FAIL 0.)

- **A lifted closure capture is a value-class representation boundary too.** A value-class parameter
  entering an outer `FunctionN.invoke` is boxed, while a nested lambda implementation's leading
  capture parameter uses the value class's unboxed carrier. The JVM value-class pass inserts the
  conversion at the `IrExpr::Lambda` capture edge; the inline splicer evaluates a resulting
  `unbox-impl(GetValue(...))` into its lambda scratch frame before emitting the body. This keeps both
  materialized and inline-spliced nested closures representation-correct (`Result<T>` included).
  (`box_corpus_regression_e2e::result_value_class_corpus_cases_box_ok`; corpus
  `inlineClasses/kt45991.kt`.)

- **An argument's lambda pre-typing binds the parameter it actually fills, not its positional
  index.** A named lambda argument binds its named parameter; a syntactic trailing lambda binds the
  LAST parameter (omitted middles take their defaults). The known-signature pre-typing paired
  `args[i]` with `sig.params[i]`, so `ef("m") { … }` on
  `ef(msg: String? = null, chk: ((Int) -> Unit)? = null, action: () -> Unit)` typed the lambda
  against `chk`'s shape (a nullable FUNCTION type resolves to a plain `Ty::Fun` — reference
  nullability is a no-op there — so the fn-param branch fired) and the argument check then reported
  "Function but Function was expected" against `action`. The mapping mirrors
  `trailing_default_arg_indices` / the arg-slotting the CHECK side already used.
  (`tests/trailing_lambda_middle_default_e2e.rs`; unblocks the checker for corpus
  `fakeInlinerVariables.kt`-class `expectFailure(msg) { … }` calls — their remaining gap is the
  omitted fn-typed default's lowering.)

- **The inline expansion's argument slotting honors the trailing-lambda rule.** A syntactic
  trailing lambda binds the LAST parameter; omitted middles take their default expressions
  (substituted directly — an inline fn has no `$default` method). The positional fill previously put
  the lambda in the first free slot, so `g { … }` on `inline fun g(x: Int = 5, action: () -> Int)`
  slotted the lambda into `x`, found `action` argument-less and default-less, and bailed the file.
  Mirrors the checker's slotting (same rule, #153).
  (`inline_fn_trailing_lambda_with_omitted_default`; advances the corpus
  `fakeInlinerVariables.kt` chain to its next blocker.)

- **Generic FUNCTION-type aliases expand by use-site type-argument substitution.**
  `typealias Mapper<T, R> = (T) -> R` records its type-parameter NAMES; a use site
  `Mapper<Int, String>` clones the target and substitutes each parameter-named leaf with the
  corresponding (recursively pre-expanded) use-site argument. Function-type targets are detected by
  an `->` ahead of the end of the alias line (covers `suspend`/`context(...)`/receiver `R.() -> T`
  spellings uniformly); a class TARGET whose type argument carries the `->`
  (`Map<String, (Int) -> Int>`) keeps its plain class-name alias. An alias whose target is ITSELF a
  generic fn alias reference (`typealias Chain<T> = Mapper<T, String>` — no `->` on the line) is not
  expanded (unresolved → skip). (`generic_fun_type_alias_substitutes_use_site_args`,
  `generic_suspend_fun_type_alias`, `class_target_alias_with_fn_type_argument_is_preserved`.)

- **An UNRESOLVED local type annotation is an error, not a silent `Error` bind.** `resolve_ty` is
  deliberately lenient (returns `Ty::Error` with no diagnostic) for expression positions, but a
  local whose annotation fails to resolve would take its initializer's shape with every use-site
  check Error-suppressed — a cross-module `val b: Bar<String> = { "OK" }` (alias declared in another
  module, not importable) SAM-converts the lambda by its own arity and throws
  `IncompatibleClassChangeError` at the call expecting the annotated shape (corpus
  `typeAliasesKt13181.kt`, unlocked by the generic-alias expansion). kotlinc rejects the unresolved
  annotation; krusty now does too. (`unresolved_local_type_annotation_is_rejected`.)

- **A `suspend Bar.() -> R` value invoked with member syntax is a suspension point.** `b.f()` /
  `b?.f()` where `f: suspend Bar.() -> R` is in lexical scope resolves like the non-suspend
  receiver-fn invoke (receiver folded first), and the lowering records the `InvokeFunction` in
  `suspend_calls` so the coroutine pass threads the continuation (`Function{N+1}.invoke`) and parks
  on `COROUTINE_SUSPENDED`; the enclosing-body suspension scan recognizes the checker-selected
  `ReceiverFnInvoke` the same way it does a suspend function VALUE. Two state-machine gaps this
  exposed, both fixed generally: (a) a compiler TEMP bound to a `when` with a suspending branch
  VALUE (the safe-call materialization `val t = when { b != null -> f.invoke(b), else -> null }`)
  is force-spilled — the flattener binds it in a branch's resume state and reads it in the merge
  state, so the straight-line "suspension inside the initializer is consumed before the store"
  liveness rule does not apply; the cond-suspension detector and `emit_cond` additionally see
  through a redundant `Cast`/`ImplicitCoercion` wrapper over the branch's direct suspension (the
  boxing the safe-call lowering adds so both arms are references). (b) a resume value bound at a
  NULLABLE-PRIMITIVE type (`Int?`) gets a real `checkcast` to its wrapper (`Integer`) —
  `ImplicitCoercion` cannot unbox to a nullable and would leave the slot `Object` while the spill
  restore's frame type is the wrapper (VerifyError at the state merge).
  (`suspend_receiver_fn_param_invoke`, `suspend_receiver_fn_invoke_parks_and_resumes`,
  `suspend_receiver_fn_safe_call_invoke`.)

- **A statement-shaped conditional as a `Unit` value in a suspend body.** A `Unit` suspend
  lambda/fn whose LAST statement is an `if`/`when` containing the suspension
  (`builder { if (suspendHere() != "OK") throw … }`, the corpus `coroutines/emptyClosure.kt` shape)
  reaches the flattener as `Variable{ty: Unit, init: When}` + `return coerce(GetValue)` (lambda) or
  `return <When>` (fn). A statement-shaped `When`'s VALUE emission leaves nothing on the operand
  stack, so the consumer's `astore` underflows (VerifyError). `split_unit_conditional_returns`
  (called from both state-machine builders, before suspension hoisting) rewrites both shapes to
  `<when as stmt>` + the `Unit` singleton as the actual value — kotlinc's shape. The `return` split
  is gated on a `Unit` LOGICAL return so a value-carrying tail `return <suspend call>` keeps its
  forwarding; the `Unit`-local split is unconditional (the bind's value is always the singleton).
  `tests/suspend_unit_tail_conditional_e2e.rs`.

- **For-loop destructuring and mapped interfaces.** Destructuring resolves `componentN`
  member extensions through the ordinary implicit-receiver rules. Extension matching walks the
  federated source hierarchy and preserves applied type arguments, including source classes that
  implement classpath interfaces. Platform-provided member mappings generate forwarding bridges
  for Kotlin properties and methods with different physical names, such as `Map.keys` and
  `CharSequence.get`. Tests: `tests/for_destructuring_components_e2e.rs`,
  `tests/collection_special_member_stub_e2e.rs`.

- **The invoke CONVENTION admits a member EXTENSION `operator fun Recv.invoke`, and a supertype-
  constructor lambda argument is typed against the selected ctor's parameter.** A receiver-DSL shape
  such as `class A : DslBase({ "case" { … } })` failed twice over: **(a)** a lambda in class-header
  base args was typed with no expected type, so the DSL receiver scope never entered the implicit-
  receiver stack — base-arg lambdas are now deferred, and the ordinary constructor-delegation
  candidate/slot machinery selects the super constructor uniformly for same-file, module, and
  classpath bases. The lambda is then checked against its source argument's selected parameter type,
  including named/vararg mapping, like an ordinary call-site argument; **(b)** `record_invoke` only
  considered member `invoke` and top-level extension `invoke`, never a member extension — it now
  selects member-extension candidates in an explicit operator-only mode (a non-`operator fun
  Recv.invoke` stays rejected by call syntax), and the lowerer emits the recorded
  origin-neutral `MemberExtension` target for a call whose callee is an arbitrary expression (the
  literal `"case"`).
  Tests:
  `invoke_operator_extension_e2e::member_extension_invoke_in_super_ctor_receiver_lambda` (runs),
  `…::named_super_ctor_lambda_uses_its_mapped_parameter_type`,
  `…::sibling_file_super_ctor_receiver_lambda_uses_shared_frontend_resolution`,
  `…::classpath_super_ctor_receiver_lambda_uses_shared_resolution` (runs),
  `…::secondary_super_delegation_receiver_lambda_uses_shared_resolution` (runs),
  `…::member_extension_invoke_in_with_receiver_lambda` (runs, no ctor lambda involved),
  `…::non_operator_member_extension_invoke_not_used_by_call_syntax`,
  `…::non_operator_top_level_extension_invoke_not_used_by_call_syntax`.

- **Reference range expressions and bound-aware classpath generics.** A standalone `a..b` over
  reference operands resolves through the ordinary `rangeTo` operator path after primitive range
  handling. Classpath generic signatures preserve declared bounds for receiver matching and JVM
  erasure. Tests: `tests/reference_range_expression_e2e.rs`.

- **Source generic signatures participate in call-site substitution.** Module callables retain
  their declared type parameters, receiver, parameters, bounds, and return type. Receiver-call
  resolution uses that signature to specialize higher-order parameters, so a declaration such as
  `fun <T> Container<T>.transform(f: (T) -> T)` types `f` from the applied receiver.

- **Go-to-definition into classpath dependencies (LSP).** A reference that resolves to a
  classpath-library declaration with no source target (a top-level function such as `listOf`, an
  extension such as `String.trim`) is recorded as a `LibraryRef` (owner internal name + JVM member
  name and descriptor) alongside the source definition index. On a go-to-definition request with no
  source target, the async engine asks the restartable compiler worker to materialize the owning class
  and returns a `file://` `Location`. Materialization prefers a dependency's attached `-sources.jar`
  entry (configurable with
  `-deps-sources`/`-no-deps-sources`) and otherwise renders a browsable Kotlin stub from the resolved
  `LibraryType` plus its `@Metadata`: package, declaration keyword, type parameters, supertypes,
  member functions (with `suspend`/`inline`, extension receiver, source parameter names, return type),
  properties (`val`/`var`/`const`), enum entries, and a companion marker. Classes without Kotlin
  metadata render their resolved bytecode members. Attached sources may sit beside the classes jar
  or in a sibling Gradle checksum directory. Entries are matched by package and declaration, so
  source-set prefixes, multi-declaration files, nested classes, and facade callables resolve to the
  declaration span; `expect` declarations are fallbacks for `actual` declarations. Kotlin builtins
  use the jar containing their `.kotlin_builtins` fragment rather than the mapped JVM class jar.
  Materialized text is cached under a content key in a format-versioned directory
  (`$XDG_CACHE_HOME/krusty/deps/v<N>/`) and garbage-collected by access age and total size. Tests:
  `crates/krusty-lsp/tests/deps_render.rs`, `crates/krusty-lsp/src/server.rs`
  (`definition_into_a_library_returns_a_materialized_file_location`),
  `crates/krusty-lsp/src/deps_cache.rs`.

- **Newlines after infix operators continue the expression.** The right operand may begin after one
  or more newlines, while a newline before the operator still terminates the expression. A `when`
  subject declaration may likewise place its initializer after a newline. Test:
  `tests/infix_newline_operand_e2e.rs`.

- **Safe calls use the ordinary value-argument grammar and slot mapping.** Named and spread
  arguments, including defaults supported by the ordinary target, use the same member and extension
  call machinery as non-safe calls. Supplied arguments evaluate left-to-right inside the non-null
  branch, then load in parameter order. Tests: `tests/safe_call_argument_list_e2e.rs`.

- **Named extension applicability uses the composite source graph.** Overload selection does not
  distinguish a positional call from a labelled one, nor a module type from a classpath type: a
  module-declared subclass is assignable to a classpath extension parameter through the same
  federated hierarchy used by ordinary resolution. Test:
  `named_args_classpath_e2e::named_classpath_extension_accepts_a_module_subclass_argument`.

- **Generic constructor inference preserves concrete parameter shells.** A parameter declared
  directly as `T` is inference-only before `T` is bound, but a parameter such as `(Int) -> T` still
  requires a function of the correct arity, suspend shape, and nullability. Constructed types such
  as `List<T>` likewise retain their concrete head. This keeps an incompatible generic primary out
  of overload competition with a valid concrete secondary constructor. Tests:
  `definitely_non_null_type_e2e::generic_function_constructor_still_requires_a_function_argument`
  and
  `definitely_non_null_type_e2e::concrete_secondary_beats_an_incompatible_generic_function_primary`.

- **An anonymous function's `return` targets the anonymous function, everywhere.** `fun (…): T { …
  return e … }` is a LOCAL return — unlike a lambda's bare `return`, which is a non-local return from
  the enclosing function. Three seams each had to agree, and each was wrong in its own way:
  - *Checking.* The body was checked with the ENCLOSING function's return type still installed, so
    `fun(x: Int): Int { return x + 1 }` inside a `String`-returning function reported "return type
    mismatch: expected 'String', actual 'Int'". The body now runs with the anonymous function's own
    return type installed — its declared one, or `Unit`, which is what a block-bodied anonymous
    function without a declared type returns.
  - *Typing.* The declared return type (`fun (…): T`) is the function type's return; a block body
    ending in `return` types as `Nothing` and would otherwise erase the result. The plain-lambda arm
    already did this, but the two `check_lambda_with_*` arms (reached whenever an EXPECTED function
    type exists — `val f: (Int) -> Int = fun(x): Int { … }`, or a call argument) did not, so the value
    carried `… -> Nothing` and the lowered closure emitted a void `return` where its caller expected a
    value (`VerifyError: Method expects a return value`). All three now share one rule.
  - *Splicing.* A lambda's `inline_body` is its body copied into the caller — correct for a lambda,
    whose bare `return` SHOULD return from the enclosing method, and wrong for an anonymous function,
    whose `return` would then return out of the caller mid-body (`filter(fun(n: Int): Boolean { return
    n % 2 == 0 })` returned out of the enclosing function on the first element). Declining the splice
    is not an option: a classpath `inline` callee is `MustInline`, so a failed splice bails the file.
    Instead an anonymous function's `inline_body` is an `invokestatic` CALL to its own impl method —
    the splice binds value indices `0..` (captures, then the lambda's own parameters) to the slots it
    prepared, so passing those indices reproduces the closure call exactly and the impl's `*return`
    stays inside the impl. Such an impl is live despite no `invokedynamic` referencing it, so it is
    exempt from both the must-inline dead-marking and the facade dead-lambda sweep.
  - *Scanning.* "Does this body carry a bare `return`?" — the test that marks a lambda impl
    splice-only — must STOP at a nested anonymous function. Its `return` is the anonymous function's
    own, and counting it marked the ENCLOSING lambda splice-only: the impl method was dropped while
    the `invokedynamic` referencing it remained (`NoSuchMethodError`). A nested plain LAMBDA is still
    descended into, since its bare return really is non-local to the enclosing function. This one was
    latent — the corpus case that hits it (`inference/pcla/issues/kt65300f.kt`) was REJECTED by the
    front end before, so it never reached lowering; fixing the checker surfaced it. A corpus SKIP
    counts as a pass, so removing a front-end rejection can expose a backend bug with no new test
    naming it.
  Tests: `tests/anonymous_function_e2e.rs::anon_fun_local_return_targets_its_own_declared_type`,
  `::anon_fun_return_type_inferred_from_the_expected_function_type`,
  `::block_bodied_anon_fun_without_a_declared_type_returns_unit`.

- **A facade `@Metadata` record keeps a BOUNDED type parameter as a type parameter.** The metadata
  builder maps a `Ty::TyParam` to a `Type.type_parameter` reference, which is how a reader binds `T`
  from the arguments at a call site — but the facade record was built from the collected signature's
  ERASED `params`/`ret`. An unbounded `<T>` erases to `Any` and survived by accident; a bounded
  `<T : Comparable<T>>` erases to the BOUND, so a separate compilation reading krusty's own output saw
  `clampMax(v: Comparable, hi: Comparable): Comparable` and `clampMax(10, 7) != 7` was rejected with
  "operator '!=' cannot be applied to 'Comparable' and 'Int'". The record now takes the declaration's
  `generic_sig` — the same signature resolved against the SYMBOLIC type parameters, already collected
  for exactly this purpose and already used for the record's receiver — falling back to the erased form
  only for a non-generic function, which has no `generic_sig`. This is the metadata-WRITE half of the
  same rule the call site applies when inferring a bounded type parameter's return from source. Test:
  `tests/bounded_type_param_e2e.rs::bounded_type_param_roundtrips_through_krusty_metadata`, and the
  generic half of `feature_coverage_x_e2e::roundtrip_data_class_and_generic_fn` (whose data-class half
  is the per-class record described under "`@Metadata` writer — the CLASS round-trip").

- **A companion object's `private` members are in scope throughout the containing class.** Member
  access is decided on the LEXICAL enclosing chain, not the receiver chain — a nested (non-`inner`)
  class has no outer receiver at all, yet sits inside its outer class's body. On top of that, a member
  declared `private` inside `companion object` is reachable from the containing class's body and from
  every class nested inside it, at any depth (`C.ZZZ`, `C.ZZZ.Deep`), because a companion's members
  belong to the containing class's scope. That downward reach is the COMPANION's alone: a sibling
  nested class's own `private` member stays out of reach in both directions (`C.ZZZ` cannot read
  `C.Inner`'s private member, nor can the companion), and an unrelated top-level class still cannot
  reach the companion's private member. Tests:
  `resolve::tests::private_companion_member_reaches_the_containing_class_body`,
  `companion_e2e::property_inferred_from_generic_companion_method`.

- **A field-less `companion object` property is its accessors.** `companion object { val ZERO: T get()
  = … }` has no static field anywhere: it lowers to `getZERO()` (plus `setX(T)` for a `var` with a
  bodied setter) on the synthesized `C$Companion`, exactly as kotlinc emits it, and `C.ZERO` /
  `C.LEVEL = v` compile to `getstatic C.Companion; invokevirtual`. Declaring the accessors beside the
  companion's own methods gives them the same name mangling a companion method already gets, so a
  value-class-typed accessor emits kotlinc's spelling (`getZERO-dNj3LFw()I`). The property type comes
  from the declared type, or is inferred from an expression getter body the way an initializer would
  be. Accessor bodies are type-checked like any other body — without that the setter's parameter had
  no type. Because there is no field, EVERY read routes through the accessor, not only the qualified
  `C.X` form: an unqualified read from an instance method, from a companion method, or from a member
  initializer goes through the same getter (they are the reads the checker records as static-field
  reads, so one choke point covers them). Every OTHER accessor shape on a companion property — a
  getter reading `field`, a visibility-only `private set`, a `var` whose custom setter is `private`
  (the synthesized `setX` is unconditionally public, so accepting one would allow a write kotlinc
  rejects), an accessor on a `const` or delegated property — would still be emitted as the default
  static accessor with the body ignored, so those stay rejected. An unqualified WRITE to such a
  property is still an unresolved reference, as it was before. Tests:
  `companion_e2e::companion_property_custom_accessors_run`,
  `companion_e2e::computed_companion_property_reads_outside_a_qualified_receiver`,
  `feature_coverage_q_e2e::value_class_companion_function`.

- **`@JvmName` on a top-level function names the emitted method, and decides the clash.** The
  annotation's constant string is the bytecode method name; call sites still resolve by the SOURCE
  name, and each emits the annotated spelling — a same-file call and a callable reference through the
  resolved function's own name, a CROSS-file call through a module-wide table keyed by declaration,
  since that caller cannot see the callee's AST. A callable reference keeps the Kotlin name for
  reflection and targets the JVM name for its invoke. Scope: top-level FUNCTIONS with a constant
  string argument. A top-level EXTENSION is not renamed (nor is its clash key), and a non-literal
  argument falls back to the source name — both are ABI divergences from kotlinc, not miscompiles.
  Because a platform
  declaration clash is a statement about JVM signatures, the top-level overload-conflict key uses the
  emitted name rather than the source name: `fun g(x: String)` and `fun g(x: String?)` erase to one
  descriptor and conflict while both are spelled `g`, but not once `@JvmName("gNullable")` separates
  them — and, in the other direction, two distinct source names collapsed onto one `@JvmName` DO
  conflict. Overload selection is unaffected; it still keys on the source name. Tests:
  `frontend::tests::jvm_name_decides_the_top_level_clash`,
  `jvm_name_toplevel_e2e::jvm_name_is_emitted_for_every_call_path`,
  `resolve_parse_deep_coverage_e2e::overload_by_nullability`.

- **A property reference carries its type arguments.** `::p` / `obj::p` is `KProperty0<V>` (or
  `KMutableProperty0<V>`) and `Type::p` is `KProperty1<T, V>`, not the raw class — so `get()` reports
  the property's own type and a member read on the result (`p.get().value`) resolves. Every reference
  form supplies them: top-level, implicit-`this`, bound member, bound extension, unbound member,
  unbound extension, object, and classpath. The arguments are semantic only; emission is unchanged
  (annotating the result with its type already compiled before this). A reference whose arguments
  cannot be determined stays raw rather than binding a wrong type, and so does one whose property
  type the reference lowering cannot realize — a VALUE-class-typed property, whose accessor is
  mangled (`getZ-<hash>`) and which the synthesized reference class does not spell (both flavours
  count: a source `@JvmInline` class and a CLASSPATH one such as `UInt`, which is why the test asks
  the provider as well as the source table), or a property
  typed as a function WITH a receiver or context parameters, which is not realized as a plain
  `FunctionN` there. Keeping the checker in lock-step with the lowerer that way leaves those cases
  as clean skips instead of a `NoSuchMethodError`/`ClassCastException` at run time. Tests:
  `mutable_property_ref_e2e::property_reference_get_reports_the_property_type`,
  `toplevel_property_ref_e2e::toplevel_property_refs_run`.

- **Compiler-realized property reads are one list, shared by checking and signature inference.**
  `"s".length`, `c.code`, and an array's `size` are realized directly rather than through a declared
  getter — `Char.code` in particular resolves through no getter at all, since `Char` is a primitive
  and `code` is a stdlib extension. The checker and the signature-phase initializer inference read
  the same `intrinsic_property_read` list, so a top-level `const val code = a.code` infers `Int`
  instead of reporting "cannot infer the type of property"; before, the identical read type-checked
  inside a function body or under an explicit type annotation but not when a top-level property's
  type had to be inferred from it. Test:
  `toplevel_property_inference_e2e::toplevel_property_cross_reference`.

- **A failed property inference has one diagnostic owner.** If an initializer or getter already
  reports its error, the declaration does not add a `cannot infer the type` diagnostic and later
  reads of the error-typed property remain quiet. Deferred inference records a failure against the
  source declaration: recursion is reported at each recursive body, and every same-file forward
  read in an eager initializer reports that the variable must be initialized. An untyped block
  getter still reports its required explicit type before any body diagnostic. Tests:
  `tests/cannot_infer_cascade_e2e.rs`, `tests/diagnostics_match_kotlinc.rs`.

- **A classpath value class's member property is read through its static `-impl` accessor.** kotlinc
  realizes every member of a `@JvmInline value class` as a static whose FIRST parameter is the
  receiver's carrier (`kotlin/Result.isSuccess` → `isSuccess-impl(Ljava/lang/Object;)Z`,
  `Celsius.label` → `getLabel-impl(I)Ljava/lang/String;`). Three facts have to line up for such a read
  to resolve and verify. (1) The metadata query drops that carrier parameter, so the property presents
  the zero-parameter accessor an ordinary class exposes — but NOT for the value class's own sole
  property, which IS the carrier and keeps its ordinary instance getter (`getDegrees()I`). (2) The
  property's declared type comes from the decoded primitive rather than a re-boxed class name, so it
  agrees with the accessor's unboxed return. (3) At emit, a static accessor consumes the receiver
  exactly when it is such an `-impl`; a `@JvmStatic` object property's static `setX(V)` takes a VALUE
  in that slot, not a receiver, and the receiver it does consume is narrowed to the accessor's declared
  carrier, never to the value-class box (no unboxed carrier passes `checkcast kotlin/Result`).
  Symmetrically, the JVM pass must not box the receiver of such a read: boxing is right for a value
  class krusty itself compiles (whose computed property is an instance accessor on the box) and wrong
  for a classpath one. Tests:
  `classpath_value_class_member_e2e::classpath_value_class_member_property_reads_through_impl_accessor`,
  `feature_coverage_n_e2e::result_is_success`.

- **A lambda converted to a `fun interface` realizes the interface's DECLARED slots, not `FunctionN`'s.**
  A plain Kotlin lambda reaches its body through `FunctionN.invoke`, whose slots are generic, so a value
  class travelling through one is BOXED. A SAM conversion targets a declared method instead, and a slot
  the interface spells as the value class itself erases to the class's underlying — kotlinc's
  `ResultHandler.onResult(Ljava/lang/Object;)` carries the *carrier*, not a `kotlin/Result` box. The
  lowerer records the SAM method's declared parameter and return types (`IrFile::lambda_sam_signature`)
  and the JVM pass decides per slot: declared-as-the-value-class ⇒ carrier, anything else (a type
  parameter, or no SAM at all) ⇒ box, as before. The same declaration drives the return: such a lambda's
  impl method keeps its erased return and its tail is neither boxed to `X` nor run through the generic
  value-class tail boxing. Two further consequences of the interface method mangling
  (`onResult` → `onResult-d1pmJ48`): the `invokedynamic` must name the MANGLED method, or the closure
  implements nothing the interface declares (`AbstractMethodError` at the first call); and a call to such
  a method already yields the carrier, so the cast to the declared type the lowerer wrapped it in — it
  types calls before any erasure is known — is stripped rather than read as proof the result is a box.
  With those in place the checker no longer refuses a `fun interface` whose method mentions a value
  class. Tests: `fun_interface_value_class_e2e` (parameter, return, scalar underlying, and the generic
  slot that must still box), corpus `inlineClasses/funInterface/{argumentResult,returnResult}.kt`,
  `inlineClasses/kt44141.kt`.

- **A `Nothing`-bodied lambda materializes as an ordinary closure.** A lambda whose body diverges is
  typed `-> Nothing`, and krusty skipped the whole file on one that did NOT diverge through a bare
  non-local `return` — which is what made `runCatching { throw … }` uncompilable. Nothing about the
  shape needs modelling: the closure's impl method simply never falls off its end, so the existing
  diverging path emits it. One correction to the declared type is owed, though. A body that leaves
  ONLY through the lambda's own `return@label` is also typed `Nothing` — it never falls off its end —
  yet it still produces that return's value and the closure method is what returns it; taking
  `Nothing` literally emits a void `return` with the value still on the operand stack ("Method expects
  a return value"). The labelled returns' common type is recovered and used as the closure's return.
  A body whose returned value that recovery cannot type — an IMPLICIT label (`build { return@build … }`,
  not spelled on the lambda) or a valueless `return@label` in a `Unit` lambda — still skips, since it
  would emit exactly that void return; a body that diverges without returning at all is unaffected,
  never reaching a return instruction. Tests: `diverging_lambda_e2e`,
  `feature_coverage_n_e2e::result_is_success`; corpus `labels/infixCallLabelling.kt` and
  `coroutines/nonLocalReturn.kt` are the shapes still skipped.

- **A CLASSPATH class's member extensions resolve like a source class's.** `ClassSig::member_ext_funs`
  is populated from source syntax alone, so a dependency's
  `class DslScope { operator fun String.invoke(body: () -> Unit) }` was invisible and `"x" { … }`
  inside a `DslScope.() -> Unit` lambda reported "expression is not callable" — with or without a
  constructor in the picture; the super-constructor spelling merely happened to be the reported one.
  Three facts have to be recovered from the dependency's `@Metadata`, none of which the class file
  carries. (1) That the member IS an extension: on the JVM it is an ordinary instance method whose
  first parameter is the receiver (`DslScope.invoke(String, Function0)`), indistinguishable by
  descriptor from an ordinary member taking a `String`. (2) That it is `operator`, without which call
  syntax would accept a plain member extension. (3) Its value parameters' names and defaults — the
  argument mapping takes its parameter COUNT from those names, so an empty list made a trailing lambda
  look like an argument past the end. All three come from the member-extension `MetaFn`, matched by its
  exact recorded descriptor: the shared member alignment deliberately excludes extensions, because
  their metadata parameter list omits the receiver the JVM method leads with. The DISPATCH receiver
  requirement is preserved by construction — the recovered signature is consulted only while walking
  the implicit receivers in scope, exactly as a source one is, so `"x" { }` still does not resolve
  where no `DslScope` is in scope. Tests:
  `invoke_operator_extension_e2e::{classpath_member_extension_resolves_in_a_plain_receiver_lambda,
  non_operator_classpath_member_extension_is_not_used_by_call_syntax,
  classpath_super_ctor_receiver_lambda_uses_shared_resolution}`.

- **A callable reference is a `KFunction{N}` where kotlinc's reflection type is observable.** kotlinc
  types `Sample::decode` as `KFunction2<Sample, Marker, String>` — a `Function2` that is ALSO a
  `KCallable`, which is why `.returnType` resolves on a reference but not on a lambda. Those
  `KFunction{N}` names exist in no jar (not `kotlin-stdlib`, not `kotlin-reflect`, and the
  `kotlin/reflect` builtins declare only the arity-less `KFunction`): kotlinc synthesizes them, and a
  declaration typed with one erases to `Lkotlin/reflect/KFunction;`. krusty synthesizes the same shape —
  `KFunction<R>` for the reflection members plus `Function{N}` so the value stays invocable — and
  computes a reference's function type first, re-typing it as the matching `KFunction{N}` in exactly two
  positions: where a `KFunction{N}` is EXPECTED (`fun reference(): KFunction0<String> = ::reveal`), and
  as the inferred type of an unannotated local bound to an UNBOUND reference (`val f = A::b`).
  Everywhere else the reference keeps its function type — that is the shape argument passing, SAM
  conversion, and the backend's reference dispatch are written against, and re-typing them all regressed
  reference dispatch broadly. Unbound only, because that is the set krusty realizes as a real
  `FunctionReferenceImpl`; a bound reference on a value receiver can still lower to an `invokedynamic`
  lambda, which is no `KFunction` (see `docs/IMPLEMENTATION_PLAN.md`). Invoking a `KFunction{N}` is
  typed from its type ARGUMENTS, not the erased reflection shape, so `::Greeter` invoked yields a
  `Greeter`. Tests:
  `classpath_unbound_callable_ref_e2e::classpath_callable_references_resolve_reflection_targets`,
  corpus `reflection/functions/typeParameterInReturnType.kt`.

- **A reference to a dependency's target is not re-mangled, and a generic function's metadata names its
  type-parameter return.** Two emit bugs that only a reflection READ can catch. (1) The value-class
  mangle was applied to a function reference's recorded name even when the target came from a
  dependency, where kotlinc had already mangled it — yielding `decode-X4E9McA-X4E9McA`, a method that
  exists nowhere and a signature kotlin-reflect cannot resolve. Only a target this compilation emits is
  mangled, matched on owner + name (an arity match misses a bound extension, whose mangle-relevant
  parameter list leads with the receiver). (2) An INFERRED return that is one of the function's own type
  parameters (`fun <T> foo(x: T) = x`) was recorded in `@Metadata` as the ERASED `Any`; it is now
  recovered from the declaration when the expression body IS one of the value parameters. A signature
  mentioning a type parameter also records its JVM method handle, as kotlinc does — the descriptor is
  not derivable from the proto types, and without it reflection reports "several matching members found"
  for a function that has exactly one.

- **A `data class`'s `componentN`/`copy` cover the PRIMARY-CONSTRUCTOR properties only.** `IrClass::fields`
  holds constructor properties, body properties and delegate fields together, so reading it whole made
  the `@Metadata` of `data class P(val a: Int) { val b = "x" }` advertise `component2` and `copy(a, b)`
  — neither of which the class emits. The same reading made a `data object` WITH a body property
  (`data object Config { val name = "c" }`) look like a data class and advertise `copy`/`component1`
  that a singleton never has. `ctor_param_count` is the exact slice, and a data declaration with NONE
  of those properties is exactly a `data object` (a `data class` must declare at least one). Both now
  match kotlinc's `d2` byte for byte. Tests:
  `sealed_interface_nested_e2e::data_object_has_no_copy`, `feature_coverage_x_e2e::roundtrip_data_class_and_generic_fn`.

- **A function reference's value-class mangle is applied at most once.** The mangle used to be re-applied
  to whatever name the lowerer recorded. For a DEPENDENCY's target that name is already kotlinc's
  mangled one, so a second pass produced `decode-X4E9McA-X4E9McA` — a method that exists nowhere.
  Origin cannot be the test: this pass sees one FILE at a time, so a SIBLING source file's target looks
  foreign to it while that file's own run does mangle it — declining there emitted a call to an
  unmangled method that never exists either. Idempotence is the test instead: a name that already
  carries exactly the suffix this signature would append is left alone, which a JVM method name can
  only do because kotlinc's mangle put it there. Tests:
  `classpath_unbound_callable_ref_e2e::classpath_callable_references_resolve_reflection_targets` (the
  classpath direction) and corpus `inlineClasses/callableReferences/*` (the same-compilation direction).

- **A classpath companion CONSTANT keeps its own Kotlin type.** `Byte.MIN_VALUE` and friends are read
  back from an integer `ConstantValue`, so the constant's type — not the descriptor's arithmetic
  category — decides the `IrConst` kind: `Char.MAX_VALUE` must box as `Character`, `Byte.MIN_VALUE` as
  `Byte`. Read as an `Int`, `Byte.MIN_VALUE` boxed to `Integer(-128)` and compared UNEQUAL to the same
  value held in a `byte` field (`incMaxByte.id() != Byte.MIN_VALUE` answered "Fail"). Both companion-
  constant paths now go through one narrowing helper. (`Char.MIN_HIGH_SURROGATE` and friends stay raw
  `u16` code units — legal code units that are not valid code points.) Corpus
  `evaluate/intrinsicConst/incDec.kt`.

- **A suspension reached through `super.f(…)` skips the file.** The state machine would have to thread
  the continuation through a non-virtual dispatch and resume back into it; the resume path does not
  model that, and the resumed frame read back `null` — the driving `Continuation` swallowed the NPE and
  the box answered nothing. Gated as `gate:suspend-super-call`. Corpus
  `coroutines/suspendFunctionAsCoroutine/superCall*.kt`.

- **A sibling-file `suspend` callee is never spliced.** `inline` on the declaration does not change the
  cross-file ABI: kotlinc emits the same `plusOne(int, Continuation)` method for an `inline suspend fun`
  as for a plain one, plus a private `$$forInline` copy it splices only inside the declaring
  compilation. The selected-call capabilities therefore report no inline-ness for a `suspend` module
  EXTENSION, so the suspend-lambda safety gate no longer refuses a call that is in fact reached through
  its real CPS entry point. A same-file `suspend inline` member still reports it and still gates.
  Tests: `cross_file_inline_call_e2e::suspend_inline_extension_cross_file_executes`,
  `coroutine_intrinsics_e2e::suspend_inline_operator_*_reaches_the_inline_gate`.

- **A fully-qualified call with an explicit type argument and a trailing lambda over a defaulted
  leading parameter (`kotlin.test.assertFailsWith<E> { … }`).** The failure exposed three semantic
  handoffs that had accidentally depended on source spelling. (1) After top-level selection, every
  unlabelled `$default` call now publishes one argument-to-parameter slot map: positional arguments
  fill from the front and a syntactic trailing lambda fills the final slot. The checker and lowerer
  consume that shared map instead of the FQ channel pairing arguments index-for-index (which checked
  the lambda against `message: String?`). (2) Explicit call type arguments are published through one
  spelling-independent helper, and all receiver-less calls reach one reified static-call boundary.
  That boundary performs substitution and then retains the existing origin router, so a source-module
  facade stays a source-module call while a classpath facade stays a library call. (3) Receiver-less
  intrinsics are dispatched from the selected callable for both bare/imported and FQ spellings, rather
  than giving `assertFailsWith` an FQ-only branch. The inline emitter also passes the same reified
  substitution into each `splice_unified` attempt. This keeps default-slotting, intrinsic behavior,
  reification, and module/classpath origin orthogonal. Test:
  `tests/fq_targ_trailing_lambda_e2e.rs`.

- **Annotation-class retention is stamped on the compiled annotation interface, kotlinc-style.** An
  `annotation class` records its declared Kotlin retention as meta-annotations in its own
  `RuntimeVisibleAnnotations`: an EXPLICIT `@Retention(X)` stamps `kotlin.annotation.Retention(X)`
  first, and every annotation class carries `java.lang.annotation.Retention(RUNTIME|CLASS|SOURCE)`
  (RUNTIME when defaulted, CLASS for Kotlin BINARY). The java stamp is the channel consumers — the
  JVM, javac, and krusty's own classpath reader (`LibraryType::retention`) — read the retention back
  from; without it a krusty-built annotation lib made every use-site annotation drop (retention
  unreadable → treated as unusable). IR: `IrClass::annotation_retention: Option<AnnoRetention>`
  (`Default` ≠ explicit `Runtime`: kotlinc omits the kotlin stamp when the retention is defaulted).
  Test: `tests/classpath_annotation_emit_e2e.rs` (krusty-built lib by default).

- **A class annotation lands in the attribute its retention selects, for every declaration kind.**
  A RUNTIME-retained annotation applied to a class goes to the class's `RuntimeVisibleAnnotations`;
  a BINARY-retained (Kotlin `AnnotationRetention.BINARY`, Java `CLASS`) one goes to
  `RuntimeInvisibleAnnotations`, which the emitter writes directly after the visible attribute. Both
  hold for every kind a class file can be — class, object, interface, enum, annotation class — each
  of which has its own emitter. `@ApiStatus.Internal` is the common case: BINARY-retained, so
  dropping the invisible attribute silently discarded it. Common IR uses one
  `DeclarationAnnotations` shape for classes, functions, constructors, and fields; every entry keeps
  the resolved semantic retention. SOURCE-retained annotations are absent, and the JVM class writer
  alone partitions the remaining entries into visible and invisible physical attributes.
  Test: `tests/class_annotation_attributes_e2e.rs` (differential, all five kinds).

- **Top-level function annotations survive into `@Metadata` `Function.annotation` records.** An
  argument-less BINARY/RUNTIME-retained annotation applied to a top-level function is recorded as
  `Function.annotation` (field 12) `Annotation { id }` with the class in the string table's
  DESC_TO_CLASS_ID form, plus `Function.flags` `HAS_ANNOTATIONS` (bit 0) — exactly kotlinc's shape
  (probe: `choose` with `@kotlin.internal.LowPriorityInOverloadResolution` → `f12 { f1: <id> }`,
  flags `7`). This is the channel a separate compilation reads resolution markers from
  (`MfnFlags::low_priority` via `has_annotation`). SOURCE-retained annotations (`@Suppress`) are
  dropped from metadata, matching kotlinc; annotations WITH arguments are not yet modeled and are
  omitted rather than recorded argument-less (a wrong record is worse than none). Test:
  `tests/classpath_annotation_emit_e2e.rs::classpath_low_priority_annotation_reaches_overload_selection`.

- **Member extension properties are metadata `Property` records, not accessor `Function`s.** A member
  extension property (`object Tools { val Int.doubled get() = … }`) lowers to accessor METHODS
  (`getDoubled(I)I`), but its class `@Metadata` record must be a `Property` carrying
  `Property.receiver_type` (f5) and the accessor `JvmPropertySignature` — kotlinc emits NO `Function`
  record for the accessor. Krusty previously recorded the getter as a member extension FUNCTION
  (`getDoubled` + `$receiver`), so `import Tools.doubled` from a krusty-built classpath was
  `unresolved reference` while sibling extension FUNCTIONS resolved. The declaration facts ride
  `IrFile::member_ext_props` (semantic receiver/type + accessor fids, per class) — the accessor fids
  are excluded from the declared-function records and re-emitted as `Property` records with
  `receiver` (`metadata::class_builder::PropMeta::receiver`). Test:
  `tests/classpath_object_member_extension_import_e2e.rs` (krusty-built dependency by default).

- **Declared secondary constructors are described in class `@Metadata`.** A class with secondary
  constructors previously published NO metadata at all (blanket admission bail), so a krusty-built
  `class Dual { constructor(a: Int, f: Cfg.() -> Unit); constructor(a: String, g: (Int) -> Unit) }`
  was not a Kotlin class to consumers — `unresolved function 'Dual'`. Each DECLARED secondary
  constructor now emits a `Class.constructor` record (flags 22 = public + `IS_SECONDARY`, kotlinc
  2.4.0) built from `IrSecondaryCtor::named_params` — the SOURCE names paired with checker-resolved
  SEMANTIC types, recorded at lowering because the erased realization loses fun-type shapes
  (`Cfg.() -> Unit` erases to a bare `Function1`). Synthetic constructors (`@Serializable`
  deserialization) get no record, matching kotlinc. A class with ONLY secondary constructors emits
  no primary record; an `enum class` without a declared constructor still records the implicit
  private `(String, I)` one (byte-identity test pins this). Value classes with secondary
  constructors keep declining (static `constructor-impl` overloads unmodeled). Test:
  `tests/classpath_ctor_receiver_lambda_e2e.rs` (krusty-built dependency by default).

- **Primary-ctor varargs and non-derivable member descriptors survive into class `@Metadata`.** A
  `vararg` primary-constructor parameter records `ValueParameter.vararg_element_type` (f4) — without
  it a consumer demands a literal array argument and rejects `Words()` ("no value passed for
  parameter"). `IrCtorArg::is_vararg` carries the fact from lowering. A member function record also
  carries its physical `JvmMethodSignature` descriptor whenever a reader could not derive it from
  the proto types: a signature mentioning a TYPE PARAMETER (`fun <T> genericJoin(vararg parts: T)`
  erases to `[Ljava/lang/Object;`, which nothing in the record names) or a vararg member — kotlinc
  records both. Derivable signatures keep omitting it. Tests:
  `tests/interface_supertype_members_e2e.rs`, `tests/named_args_classpath_e2e.rs` (both krusty-built
  by default).

- **Members mentioning enclosing-class type parameters publish their semantic shape.** A non-generic
  member whose declared types mention a CLASS type parameter (`open class Base<T> { open fun
  choose(value: T): T }`) lowers to an erased `IrFunction` (`Any`), and `IrFile::signatures` only
  describes function-OWNED type parameters — so the class `@Metadata` published `choose(Any): Any`
  and a consumer rejected a `Base<String>` override with "return type mismatch: expected 'String',
  actual 'Any'". Lowering now records the checker-resolved shape in
  `IrFile::member_semantic_sigs` (fid → semantic params + ret) and the class metadata prefers it,
  encoding `Type.type_parameter` references against the class table. Semantic type-parameter
  identities are checker-generated (`\0tp:…`), so the mention test is "any type variable at all"
  (`ty_mentions_any_param`) — with no function-owned parameters and no receiver, any type variable
  is an enclosing-class one. Extension members are excluded (their `params[0]` receiver alignment
  is a separate channel). Also fixed the same way: generic member-extension lambdas and one
  generic-suspend shape. Test: `tests/superclass_bridge_e2e.rs` (krusty-built by default).

- **Value-class-rewritten top-level functions record their mangled JVM handle.** A top-level function
  with a value-class parameter realizes as a MANGLED method (`taggedOnly(tag: Tag)` →
  `taggedOnly-rnqsQGE(Ljava/lang/String;)`), neither name nor descriptor derivable from the declared
  facade record — kotlinc records both in the `JvmMethodSignature` (name f1 + desc f2). The facade
  writer now recovers them from the value-class pass's `vc_declared_sigs` table (declared name +
  arity → the post-pass `IrFunction`'s physical name/descriptor;
  `facade_package_metadata_with_ir`), so a consumer can map the record to bytecode — previously
  every such function was `unresolved function` from a krusty-built classpath.
  `FnMeta::jvm_name` carries the f1 name (written only when it differs from the Kotlin name).
  Test: `tests/classpath_value_class_param_e2e.rs` (krusty-built by default).

- **Classes with value-class constructor parameters publish full metadata; secondary VC ctors get
  the marker ABI.** The blanket "no @Metadata for a value-param ctor class" decline is lifted — the
  bytecode already carried kotlinc's ABI for PRIMARY ctors (private erased `<init>` + public
  synthetic marker ctor + mangled accessors), so the record now describes it: ctor params keep their
  DECLARED types (`IrFile::vc_ctor_declared_params`, captured before erasure), the ctor
  `JvmMethodSignature` names the public marker form (an inner class's leading enclosing-instance
  param included), member records ride `vc_declared_sigs` (mangled f100), and property records carry
  the mangled getter + erased field desc (already-existing channels). SECONDARY constructors with
  value-class params now get the same private+marker realization (`IrSecondaryCtor::vc_params`,
  recorded by the VC pass pre-erasure) — bytecode, same-module construction routing (`emit_new`
  matches the erased shape), and the marker-form metadata desc. Also fixed while lifting: the
  ordinary ctor record desc now spells UNNAMED leading `<init>` params (an inner class's enclosing
  instance — consumers were one slot short), and `Class.flags` records `IS_INNER` (bit 9, kotlinc's
  518). Value classes with secondary ctors still decline (static `constructor-impl` overloads).
  Byte-parity probes: `Holder`/`Overloaded` metadata byte-identical to kotlinc 2.4.0. Tests:
  build688, enum_regex_vc, nested_ctor_reordered_named_valueclass, synthetic_ctor,
  value_class_default, value_class_nullable_widen_return — all krusty-built by default now.

- **Named `object` properties realize as JVM static fields, kotlinc's shape.** A named (non-local,
  non-companion) `object`'s property backing fields are `static` on the object class: accessors are
  instance methods reading/writing `getstatic`/`putstatic`, property initializers and `init {}`
  blocks run in `<clinit>` AFTER the `INSTANCE` store, and `<init>` is a bare `super()` call —
  byte-comparable to kotlinc (probe: `object Counter { var slot = "" }` code-identical; residual
  divergence is constant-pool/method order only). Reads/writes route statically at every level:
  the synthesized accessors, `IrExpr::GetField`/`SetField` (receiver evaluated only for effects),
  and the declared-property direct-field path (`PropertyAccess::Field { is_static }`). A pure list
  of own-field stores needs no local (kotlinc's `ldc; putstatic` sequence); only an initializer
  actually reading `this` materializes INSTANCE into slot 0 (`init_body_reads_this`).
  Local/anonymous objects and companions keep instance fields (companion static hoisting to the
  outer class is a separate, upcoming relayout).

- **Reified inline functions emit real erased methods with reification markers.** A `<reified T>`
  inline fun whose reified-parameter uses are all CLASS LITERALS (`T::class`/`T::class.java`) now
  emits a standalone erased method — kotlinc's own realization: each literal lowers to
  `Intrinsics.reifiedOperationMarker(4, "T")` followed by the ERASED class constant
  (`IrExpr::ReifiedClassMarker`), the placeholder pattern every inliner (kotlinc's and krusty's)
  patches with the call-site class. Real kotlinc consuming a krusty-built lib inlines it correctly
  (pinned by `kotlinc_inlines_krusty_reified_method`). Admission is
  `reified_uses_are_class_literals`: an `is T`/`as T` (INSTANCEOF/CHECKCAST markers, unmodeled) or
  a reified name in a nested call's explicit type arguments keeps the function splice-only, as
  before. Nested splices inside an emitted body that resolve back to the enclosing `T` also emit
  the marker rather than a resolved class.

- **Reified `is`/`as` markers and body-inlined `$default` stubs.** A non-safe `is T`/`as T` on the
  emitted fn's own reified parameter lowers to `IrExpr::ReifiedTypeOp` — `reifiedOperationMarker(3)`
  + `instanceof` / `marker(1)` + `checkcast` against the erasure, kotlinc's exact placeholder pair
  (`as? T` and nullable `is T?` targets stay splice-only). The `$default` synthetic of a reified fn
  INLINES the whole body after the default fills instead of delegating — the real method throws at
  runtime by design (the marker intrinsic), so kotlinc's `$default` carries the body and every
  splicer patches it there; krusty's delegating stub left a live direct call in spliced output. The
  spurious `JvmMethodSignature` on plain inline facade records is gone (kotlinc emits none; suspend
  and type-parameter-mentioning signatures keep theirs).

- **Return-only generic suspend overrides need no erasure bridge.** The CPS rewrite gives BOTH the
  supertype declaration and the override the same physical shape — a trailing `Continuation`
  parameter and an `Object` return — so a type parameter appearing only in RETURN position erases
  identically on both sides and no bridge exists to build (probed: kotlinc emits a single
  `byId(int, Continuation)` for `class RealRepo : Repo<Cfg> { override suspend fun byId(id: Int):
  Cfg? }`). The JVM bridge pass compares the semantic parameter shapes in both its superclass and
  interface-obligation paths and skips only when a VALUE-parameter erasure difference remains;
  common lowering does not pre-classify bridge needs from source type spellings. Return-only
  generic suspend overrides therefore compile and run. Test:
  `tests/generic_suspend_member_return_e2e.rs`
  (krusty-built by default).

- **Value-class-mangled suspend functions emit their `$default` synthetic.** The CPS-appended
  `Continuation` is just another loaded parameter in the stub (kotlinc:
  `pick-<hash>$default(int, String, Continuation, int, Object)` delegating to the CPS method); the
  mask covers only the DECLARED defaulted parameters, and the stub-safe gate already restricts
  defaults to simple constants, which cannot suspend — so the previously-rejected shape is modeled
  by the ordinary facade stub emitter unchanged. Tests: `tests/metadata_kept_params.rs`,
  `tests/unsigned_classpath_call_e2e.rs` (both krusty-built by default).

- **Safe-call `invoke` on a nullable fun-typed value resolves through the invoke convention.** For
  `op?.invoke(a, b)` where `op: ((Int, Int) -> Int)?`, the ordinary member paths know no `invoke`
  member on `Function{N}` and typed the call `Error` (the whole file then bailed at SafeCall
  lowering). The checker selects the invoke convention directly for `invoke` on a `Ty::Fun`
  receiver and calls `record_invoke` with the non-null receiver — the same convention the
  call-position spelling uses — and `expr_inner_call_member` lowers the recorded
  `ExprLowering::Invoke` reached through the
  member spelling (the safe-call assembly's non-null branch delegates there). Test:
  `tests/classpath_fun_typed_property_lambda_e2e.rs` (krusty-built by default).

- **Direct `val value: T?` members retain their semantic declaration type.** The
  per-class `nullable_tparam_props` table (name → type-parameter index) records direct
  nullable-type-parameter properties, deliberately separate from `generic_props`/
  `generic_property_shapes`. The module-symbol provider exposes the resulting
  `Nullable(TyParam)` template consistently to the property, getter, and setter, while
  `applied_declared_member_prop_ty` substitutes the receiver's semantic binding for source reads
  and stable-path analysis. Scalar bindings remain semantic nullable primitives here; boxing and
  storage are backend decisions. Thus `val <T> Box<T>.maybe: T? get() = value` type-checks as
  `T?`, and a stable `ReadBox<Int>.value` narrows to `Int` after a null check.
  Tests: `tests/classpath_static_call_inference_e2e.rs` (krusty-built by default).

- **A `try` body keeps a reified fn emittable.** The splice-only body screen rejected any `try` in
  an inline fn — but a REIFIED fn is emitted as a real method (kotlinc always emits one), where a
  standalone `try` lowers exactly as in any ordinary function; only the splice-time re-lowering
  concern applies, which the emitted-body path never takes. `try` now recurses into its children
  for reified fns instead of rejecting outright (non-local returns inside its lambdas stay
  rejected). This makes the assertFailsWith shape (`inline fun <reified T : Throwable>
  failsWith(...) { try { ... } catch ... }`) a real facade method + `$default`, so the
  fully-qualified no-import spelling resolves against krusty-built libs. Test:
  `tests/fq_targ_trailing_lambda_e2e.rs` (krusty-built by default).

- **Expected-seeded constructor bindings survive interface-implementing argument evidence.** For
  `fun entries(): Bag<String, Entry> = Bag(listOf(Item("OK")))` the expected type seeds `V = Entry`,
  but the argument-evidence merge joins types over superclass chains only, so joining the `Item`
  argument (a class IMPLEMENTING Entry) collapsed `V` to `Any` — and the invariant result then
  mismatched the very expectation that seeded it. Constructor constraint collection now preserves
  the seed as each lower argument constraint is added while that argument remains assignable to it.
  Once an argument falls outside the seed, ordinary joining takes over; there is no post-merge
  recovery pass. Test:
  `tests/build840_collection_property_element_e2e.rs` (krusty-built by default).

- **Companion `val`/`var` properties are hoisted onto the OUTER class as statics.** kotlinc's
  layout, now krusty's: the backing field is `private static [final]` on the outer class
  (regardless of the property's declared visibility), initialized in the outer's `<clinit>`
  AFTER the `Companion` instance store; the companion keeps the property declaration and its
  instance accessors, which reach the field through `public static final synthetic`
  `access$get<X>$cp`/`access$set<X>$cp` bridges on the outer; an outer INSTANCE property with
  the same source name keeps the metadata/accessor name but its JVM field is suffixed
  (`result` → `result$1`). Member order matches kotlinc: `Companion` field first, `<clinit>`
  last, bridges between the instance methods and `<clinit>`. An initializer may read sibling
  companion members (`this` = the just-stored Companion instance); one that doesn't reads as a
  bare expression in `<clinit>`. Conservative subset: public, non-const, non-lateinit,
  non-delegated, plain-accessor, initialized, non-value-class-typed properties on a PLAIN class
  outer — interface/enum/value-class outers keep the previous instance layout (their emit paths
  lack the bridge synthesis). Tests: `tests/companion_member_read_e2e.rs`
  (`kotlinc_member_companion_property_field_shape` pins the kotlinc shape; krusty-built by
  default).

- **A NESTED annotation is a call, so its argument labels are the CALL's.** `@Outer(Inner(b = "BB",
  a = "AA"))` spells its inner value as an ordinary constructor call, and the parser records those
  labels positionally against the call rather than per argument the way it does for a direct
  `@Ann(...)`. Binding a nested annotation's arguments through the annotation map alone ignored the
  labels and bound positionally: with same-typed elements it SILENTLY swapped the emitted values
  (`a="BB", b="AA"` where kotlinc writes `a="AA", b="BB"`), and with differently-typed ones it
  reported spurious argument-type mismatches. The label source therefore follows the shape: a
  nested application reads the call's names by position, a direct one reads the annotation's names
  per argument. Holds at any depth and for a mix of positional and named arguments. Test:
  `tests/annotation_emission_e2e.rs::nested_annotation_named_arguments_bind_by_label`.

- **Annotation declarations publish one normalized application shape.** The checker consumes
  semantic element identities, types, defaults, vararg position, and positional-argument policy;
  it does not ask whether a declaration came from source, Kotlin metadata, or a Java classfile.
  Kotlin constructors use their ordinary source parameter list. At the JVM provider boundary, a
  constructor-less Java `@interface` becomes the same `ParamList`: `AnnotationDefault` presence
  marks an optional element, descriptor width preserves `byte`/`short` annotation tags, a scalar
  element named `value` alone accepts one positional argument, and an array-typed `value` accepts
  positional elements as a vararg. Other Java elements are named-only. Omitting a Java array
  element emits nothing so its declaration default remains effective; omitting a Kotlin
  `vararg val` materializes the empty array required by Kotlin semantics. Unsupported parsed
  annotation values remain explicit AST nodes and are diagnosed only if an emitted annotation
  application consumes them—never as fabricated names and never at inert property, field,
  parameter, or local positions. Tests: `tests/annotation_emission_e2e.rs`.

- **Shared diagnostic text uses the Kotlin frontend's emitted wording.** Template extraction is an
  audit lead, not proof that a source construct emits that template: tests compile the same source
  with krusty and kotlinc and compare their complete diagnostics. The verified messages are
  `'break' and 'continue' are only allowed inside loops.`, `multiple vararg parameters are
  prohibited.`, `'return' is prohibited here.`, `'this' is not defined in this context.`, `cannot
  access '<this>' before the instance has been initialized.`, and `'{name}' overrides nothing.`
  Tests: `tests/diagnostics_match_kotlinc.rs::shared_diagnostic_wording_matches_kotlinc` and
  `tests/diagnostics_match_kotlinc.rs::prohibited_script_returns_match_kotlinc`.

- **A Java `@interface` element of type `Class` is Kotlin-facing `KClass`, and non-null.** The
  element's own type is `java.lang.Class`, but the use site spells it with a Kotlin class literal
  (`@Replaces(Impl::class)`), whose type is `KClass`. Presenting the JVM type rejected every such
  application with "actual type is 'reflect.KClass<..>', but 'java.lang.Class!' was expected". The
  expectation is NON-NULL despite the platform Java type — kotlinc expects `KClass<*>` and rejects
  `@One(null)` — so the mapping drops the platform flexibility rather than preserving it. An array
  element maps elementwise (`Class[]` → `Array<KClass>`). The Java method's generic `Signature` is
  authoritative for the type argument: `Class<? extends Runnable>` becomes bounded `KClass`, so
  `String::class` remains invalid. Emission is unchanged: the value is a class constant either way.
  Tests:
  `tests/annotation_class_element_e2e.rs::a_java_class_element_accepts_a_class_literal`,
  `…::the_emitted_class_constants_match_kotlinc`.

- **A missing context argument names its parameter, and a loop's `hasNext` belongs to its iterator.**
  The emitted diagnostics below are checked directly against kotlinc; extracted templates are only
  audit leads. A failed context lookup carries the parameter index and type from selection, while
  provider-normalized property records retain the corresponding source names. kotlinc reports
  `no context argument for 'c: C' found.` — naming the missing context declaration rather than the
  function or property that needs it — and the loop range's boolean check is reported as
  `the 'iterator().hasNext()' function of the loop range must
  return 'Boolean', but returns '{0}'.`, because the offending declaration is reached through the
  range's `iterator()`, not written at the call site. The invalid operator declaration is also
  diagnosed at its `operator` modifier. A Java `@interface` whose elements are
  named-only reports `Only named arguments are available for Java annotations.`, naming the reason
  the positional form is unavailable.
  Tests: `tests/context_and_loop_wording_e2e.rs`, `tests/annotation_emission_e2e.rs`.

- **A deferred local declaration's grammar does not depend on nullability.** `val x: T` with no
  initializer is a deferred assignment, whether `T` is nullable or not. The parser therefore keeps
  both forms on the same AST path; the checker owns the declared semantic type and assignment
  narrowing. The current local representation synthesizes the type's default value and makes the
  deferred slot writable for lowering. Rejecting only a nullable spelling in the parser produced
  `expected '='` for source kotlinc accepts, including IntelliJ's deferred
  `val ranges: List<MatchedFragment>?` assigned in both arms of an `if`.
  Tests: `tests/deferred_nullable_val_e2e.rs`.

- **An out projection is consumed when a callable output becomes a value.** Everything read out of
  `List<out Range>` is a `Range`. A projection is a generic-argument constraint, never a top-level
  expression type, so generic extension return specialization consumes `OutProjection` in output
  position: `list.first()` becomes `Range` before ordinary member/property selection. Without that,
  `list[0].startOffset` resolved (the indexed-member path already specializes output position) but
  `list.first().startOffset` did not. `Ty::kotlin_class_internal` remains a strict identity query and
  does not turn an invalid projected expression into a usable class as a fallback.

  Inferring a type parameter FROM a projected receiver is a separate, still-open gap:
  `list.map { it.startOffset }` does not bind `map`'s `T` from `List<out Range>`, so the candidate is
  inapplicable and the lambda body is checked against nothing. Unwrapping the projection in
  `unify_ty_impl` binds it, but regresses `Holder<*>::identity`, because a star projection is built
  with `Any?` as its bound (`resolve.rs`'s `projected_typeref_argument` call sites) rather than the
  type parameter's declared bound — so the unwrapped binding then fails the declared `T : CharSequence`
  constraint. The star's bound has to carry the declared bound before that inference can be fixed.
  Tests: `tests/projected_receiver_extension_e2e.rs`.
- **An array-literal annotation argument is folded against the element's DECLARED type, never
  desugared where it is parsed.** Which array `[1, 2]` denotes follows the element it is passed to
  — `intArrayOf` for an `int[]` element, `arrayOf` for `String[]` — and kotlinc rejects the
  mismatched factory (`@Arr(xs = arrayOf(1, 2))` is a type error against `int[]`), so the parser
  cannot choose one. The literal retains its element expressions and they are both CHECKED and folded where
  the declared type is known. Checking them there is what makes the rest work: nothing else visits
  those expressions, so a class literal or enum entry inside a literal would never resolve
  (`ks = [String::class]` was rejected while `ks = arrayOf(String::class)` was not), and an
  element-typed expectation would silently accept an array. That last case is a VARARG element
  passed POSITIONALLY, which expects the element type rather than the array: kotlinc rejects
  `@V([1, 2])` for `byte[] value()`, and folding without checking wrote `I` tags into a `byte[]`
  element, which throws `AnnotationTypeMismatchException` on read-back. A literal in a position
  krusty does not emit stays inert. Tests:
  `tests/annotation_array_literal_e2e.rs::an_array_literal_element_takes_its_declared_array_type`,
  `…::a_positional_vararg_element_rejects_an_array_literal`,
  `…::class_literals_inside_an_array_literal_resolve`,
  `…::an_array_literal_in_an_unemitted_position_stays_inert`.

- **Class initializer inference selects declared members through the module symbol source.** A
  property such as `val parts = split(pattern)` may refer forward to an explicitly typed member,
  and the member receiver rung outranks a same-named top-level function. Signature collection now
  publishes those callable headers on the classifier before it infers properties, then replaces the
  header with the complete class signature. Initializers therefore use the ordinary
  `ModuleSymbols` candidate family and `SymbolResolver` overload/generic selection for bare calls,
  explicit `this`, companion receivers, and overloads. The old top-level/member/local
  `name -> return type` maps and the “selection failed, try a spelling” paths are gone.
  Tests: `tests/member_property_inference_e2e.rs`.

- **An annotation's implementation class exists per CONSTRUCTING FILE, not per declaration.** Kotlin
  lets an annotation be instantiated (`Marker("x")`), and kotlinc realizes that with a synthetic class
  implementing the annotation interface and `java.lang.annotation.Annotation`. It emits one per
  annotation per source file, named after the first emitted lexical classifier containing such a call,
  or the file facade when none does (`Ann2Kt$annotationImpl$pkg_Marker$0`), and emits NOTHING for an
  annotation that is only declared —
  which is nearly every annotation. krusty emitted one per declaration, named `Marker$annotationImpl`,
  so any file declaring an annotation carried a class file kotlinc never writes. Common IR now tags the
  ordinary checked `New` identity with the normalized annotation declaration and lexical scope. The JVM
  realization pass groups those calls by annotation, emits the implementation at the constructing file,
  and assigns kotlinc's physical name. An annotation used only as an ANNOTATION ARGUMENT
  (`@Outer(Inner("x"))`) has no `New`, so it correctly emits no implementation.
  Tests: `tests/annotation_impl_class_e2e.rs`.

- **`Nothing` descriptors as `java.lang.Void`.** `Nothing` is uninhabited, so no value ever carries
  its descriptor — but it is written into signatures (`fun fail(): Nothing`, a `Nothing` getter), and
  kotlinc writes `Ljava/lang/Void;` there, not `Ljava/lang/Object;`. A caller compiled against
  kotlinc's ABI links against that descriptor, so emitting `Object` made every `Nothing`-returning
  helper — of which intellij-community and the stdlib have many — a different method. The class-name
  map already carried `kotlin/Nothing` → `java/lang/Void`; the type-descriptor path collapsed
  `Nothing` into `kotlin/Any` alongside the `Null` and `Error` sentinels, which are not source types
  and stay as they were.

  Parameters, fields, and constructor arguments use one declared-slot representation rule:
  semantic `Nothing` becomes the one-slot reference `java.lang.Void`. Expression lowering still
  retains non-null `Nothing` as the bottom type so calls returning it terminate control flow.
  A declared `Nothing?` maps to the same nullable `Void` reference and does not diverge; its only
  value is `null`. Value-flow lowering keeps the null-only bottom type erased to `Object`, because it
  can also arise as the inferred result of a generic call such as `choose(null, null)` whose physical
  declaration returns `Object`. This matches kotlinc for top-level, member, local, module, and
  classpath declarations without origin-specific descriptor paths.
  Tests: `tests/nothing_descriptor_e2e.rs`.

- **A construction's argument labels map onto parameter slots.** A constructor's parameter shapes
  live in DECLARATION order while its arguments are written in source order, and labels reorder them
  (`Conv(g = …, f = …)`). The parameter an argument fills is therefore a mapping, never its written
  position: reading the shapes by position hands each lambda another parameter's function type,
  which compiles perfectly well and throws `ClassCastException` at run time — with nothing generic
  in sight, so this is not a generics rule. Constructors from local, module, and classpath providers
  are already normalized into one classifier record. Their early lambda expectations now consume
  that record and the ordinary callable argument mapper—the same named/default/vararg/trailing-lambda
  rules used for final candidate selection—instead of maintaining a constructor-only mapping.
  Tests: `tests/construction_argument_labels_e2e.rs`.

- **A constructor's own type parameters are substituted before a lambda argument is checked.** A
  generic class whose constructor takes a function over its type parameters
  (`class Store<ROOT, DOMAIN>(wrapper: Wrapper<ROOT>, toDomain: (ROOT) -> DOMAIN)`) supplies that
  parameter as the lambda's expectation, and the expectation is the DECLARATION's template. The type
  arguments the call already fixes — an explicit `Store<A, B>(…)`, the expected result, and the
  arguments that are not themselves contextual — are substituted into it first. Handing the lambda
  the unsubstituted `(ROOT) -> DOMAIN` types its parameter as the erased bound, every member read in
  the body is "unresolved reference", the failed body then contributes nothing back, and the
  construction collapses to `Store<Any, Any>` so every later call on the value cascades. Explicit
  type arguments are applied OVER the inferred ones rather than merged with them: an argument whose
  own type is still open (`Wrapper("x")`) must not rebind what the call site spelled out.

  Arguments are typed in DEPENDENCY order, not source order: a contextual (lambda) argument is typed
  after the arguments that can bind the type parameters its own parameters mention, and a lambda's
  RESULT then binds the parameters that appear nowhere else. `Store(toRoot = { list -> … },
  toDomain = { dto -> … }, …)` therefore types `toDomain` first, because `DOMAIN` is visible only in
  that lambda's result. Lambdas whose inputs never become known keep source order. The same semantic
  substitution applies to ordinary, local, sibling-source, and dependency classifiers, including
  value-class type arguments; their physical representation remains a backend decision.
  Tests: `tests/generic_ctor_lambda_targs_e2e.rs`.

- **A property's constructed type is inferred from any declaration origin, but only where the
  constructor certainly owns the call.** The signature pre-pass reads a constructor template for a
  class declared in the file being collected, in another file of the module, or in a dependency —
  the file first, because its own classifiers are not published to the module table while it is
  being collected. Each origin answers with what it knows: the file's AST carries real defaults and
  `vararg` marks, a module signature carries parameter types only, a dependency carries its call
  signature. A lambda argument is contextual here exactly as in full checking — the concrete
  arguments are typed first, their bindings substituted into the parameter template, and each lambda
  body then inferred under that shape so its RESULT can bind what appears nowhere else.

  The pass performs no overload resolution, so the template applies only where nothing else can own
  the call, and that question is about the ARGUMENTS, never a parameter count: a default makes a
  shorter call reachable, a vararg longer ones, and two candidates of the same arity are separated
  only by their parameter types. "Could take this call" is ordinary SUBTYPING — a `Number` parameter
  takes an `Int` argument — asked of the primary and of every secondary alike, and answered YES
  wherever the pass cannot tell (a type variable, an erased top, a function type, an unresolved
  type). A secondary withdraws the template when it could take the call; `Wrapper("hi")` cannot
  reach `constructor(list: List<T>, index: Int = 0)`, so that one does not withdraw. Kotlin's
  preference for a non-vararg candidate applies only among APPLICABLE ones, so a vararg secondary is
  set aside only where the primary itself takes the call by type as well as arity — a module
  signature cannot tell a `vararg` from an ordinary array parameter, and guessing the other way
  commits a template the primary cannot honour.

  A same-named top-level function withdraws the template only when it SELECTS for these arguments.
  So does an explicit import of that simple name naming a non-classifier (`import other.makeCell as
  Cell` reads exactly like a construction), and a companion object an `invoke` can apply to —
  declared or inherited, by selection on the companion's own type, or an `invoke` EXTENSION whose
  receiver is that companion, which selection cannot attribute during collection but the module's
  receiver-keyed extension index can answer. A companion with no `invoke` of its own — constants, a
  `serializer()`, a logger — keeps the template, as does an unrelated `fun invoke` elsewhere.

  All of this reads the module table AS POPULATED SO FAR, so a class declared in a file collected
  later is not yet visible and the property keeps the shape it had. That makes the INFERENCE, and
  therefore acceptance, depend on collection order; it never makes it wrong, because every rule above
  withdraws on what it cannot see. Removing the order dependence needs a second pass over the
  properties that stayed open, which is separate work.

  A VALUE CLASS is kept as an inferred type argument where the construction and the declaration are
  collected together, and withheld otherwise — anywhere in the applied type, since `Box<List<Money>>`
  reaches the same erased constructor parameter as `Box<Money>`. Its JVM representation is its
  underlying value, and through a module or dependency declaration the argument arrives at that
  parameter unboxed while a read through the applied type emits `checkcast`, which does not verify.
  That lowering gap predates this inference and stays unreachable.
  Tests: `tests/generic_ctor_template_prepass_e2e.rs`.
- **`@Metadata` mirrors a declaration's applied annotations; it is not implied by the class file's
  annotation attribute.** A `RuntimeVisibleAnnotations`/`RuntimeInvisibleAnnotations` attribute makes
  an annotation work at RUNTIME, but a Kotlin consumer (and `kotlin-reflect`) reads a declaration's
  annotations back out of `@Metadata`, so kotlinc writes BOTH. krusty wrote the attribute only, which
  left an annotated member function's class file differing from kotlinc's in `d1`/`d2` alone. Member
  functions now record `Function.annotation` (f12), secondary constructors `Constructor.annotation`
  (f3), and both set the `HAS_ANNOTATIONS` flag bit (bit 0) — derived FROM the records, never an
  independent input, so a `public final` member's flags word goes 6 → 7 and a secondary constructor's
  22 → 23. The one list per declaration rejoins krusty's retention split (`FnAnnotations`'
  `visible`/`invisible`), which exists only because the class file has two attributes; kotlinc records
  every non-SOURCE annotation in one repeated field. Two ORDERS are independent and must not be
  conflated: the `JvmMethodSignature` extension (f100) INTERNS its d2 strings BEFORE the
  DECLARATION-level annotation records (f3/f12/f14) even though it SERIALIZES after them, so an
  annotated `suspend` member's CPS descriptor precedes `Lp/Mark;` in `d2` while its bytes stay in
  ascending field order. The rule is per FIELD, not "annotations vs f100": a VALUE-PARAMETER
  annotation interns with its own parameter, ahead of f100. Measured on
  `class C @OnCtor constructor(@OnParam val x: Int)` — `d2` is
  `["Lp/C;", "", "x", "", "Lp/OnParam;", "<init>", "(I)V", "Lp/OnCtor;", …]`, the parameter's
  annotation before the constructor's signature strings and the constructor's own after them. Tests:
  `tests/annotation_emission_e2e.rs::member_function_annotation_reaches_metadata` (byte-identical to
  kotlinc), `…::member_function_annotation_arguments_reach_metadata`,
  `…::binary_retained_member_annotation_reaches_metadata`,
  `…::secondary_constructor_annotation_reaches_metadata`,
  `…::annotated_suspend_member_interns_its_signature_before_the_annotation`,
  `…::annotated_suspend_top_level_function_interns_its_signature_before_the_annotation`.
  PROPERTY annotations now take their own route (below). Still DROPPED before the IR, so there is
  nothing to mirror yet (each needs the class-file side first, not just the metadata record): a
  PRIMARY constructor's own annotations (`class C @Anno constructor(…)`) and VALUE-PARAMETER
  annotations (no `RuntimeVisibleParameterAnnotations` is emitted at all).
- **A line break inside a property declaration is a continuation, an explicit `;` is not.** Kotlin's
  property grammar is `… (':' NL* type)? (NL* '=' NL* expression)?`, so a declaration whose type
  fills the line may put the type or the initializer on the next one — which is exactly what a
  formatter does to a long generic type. Ending the declaration at the newline leaves the `=` (or the
  type) to be read as the start of the next declaration, which is not a recoverable position: the
  same gap surfaced as four unrelated-looking diagnostics — "object bodies support 'fun', 'val'/'var',
  and 'init' blocks" once per token of the initializer, the class-body form of it, "expected a
  top-level declaration", and "expected an expression" for a local. Nothing else in the grammar
  begins with `=`, so looking past the line breaks cannot swallow anything but this declaration's own
  initializer. The lexer spells a line break and a `;` as the same token, so the lookahead goes
  through the helper that stops at a semicolon: `val a: Int; = 1` is two declarations, the second of
  which is not one, and reading past it would accept what kotlinc rejects. The rule is one rule
  everywhere a property is declared — top level, class, object, interface, companion, local, a
  destructuring `val (a, b)`, and a `when` subject binding.
  Tests: `tests/property_initializer_newline_e2e.rs`.
  PROPERTY annotations now take their own route (below), and the PRIMARY constructor's the one after
  it. Still DROPPED before the IR, so there is nothing to mirror yet (it needs the class-file side
  first, not just the metadata record): VALUE-PARAMETER annotations (no
  `RuntimeVisibleParameterAnnotations` is emitted at all).

- **A primary constructor's own annotations reach both halves.** `class C @Mark constructor(val x: Int)`
  parsed its annotations but dropped them: the emitted `<init>` carried no annotation attribute and the
  metadata `Constructor` record none either. The annotation names were already on the AST; their
  ARGUMENT expressions were discarded at the parse site, so both had to be carried through lowering
  (the same retention split a function's and a secondary constructor's get). Three placement facts,
  each verified byte-for-byte against kotlinc 2.4.10:
  - The flags word is not a plain OR. `Constructor.flags` is OMITTED at its proto default 6
    (visibility PUBLIC), which krusty represents as 0; setting `HAS_ANNOTATIONS` forces the field to be
    written, so the omitted default must be MATERIALIZED first — an annotated public primary ctor
    writes 7, not 1 (which would read back as visibility INTERNAL). A declared `private`/`protected`
    primary already carries a non-zero word (2 / 4) and just gains the bit.
  - The annotation type INTERNS at the constructor's own annotation visit, which ASM runs after
    `visitMethod` and before `visitParameterAnnotation`: `Lp/Mark;` lands between the ctor descriptor
    and the body's first entry, ahead of any `@NotNull` parameter annotation.
  - An ALL-DEFAULTS primary constructor has a SECOND declaration to annotate: the no-arg convenience
    `<init>()` that stands in for it. kotlinc repeats the annotation there; the synthetic `$default`
    overload between them gets none.
  Separately, `@Deprecated` on a constructor propagates the classic `Deprecated` ATTRIBUTE (not the
  annotation) to that synthetic `$default` overload — for SECONDARY constructors too, which krusty had
  also been omitting. Tests: `tests/annotation_emission_e2e.rs::primary_constructor_annotation_reaches_metadata`
  (byte-identical to kotlinc), `…::primary_constructor_annotation_arguments_reach_metadata`,
  `…::primary_constructor_annotation_reaches_the_no_arg_convenience_ctor`,
  `…::deprecated_primary_constructor_marks_its_default_overload`.
  Like the property annotations above, these are RECORDED but NOT diagnosed by the checker — krusty's
  annotation constant folder is narrower than kotlinc's, and reporting from a newly added check would
  reject sources that compile today.
- **A type variable is solved through the declaration's own bound relation.** A generic declaration
  states constraints beyond its parameter types, and both are load-bearing at a call site. For
  `fun <T : Base<T>, C : T> C.f(subs: Iterable<T>)`, an argument can pin `T` to a type its OWN bound
  forbids — `Auth().f(listOf(Login()))` pins `T = Login`, but `Login` is a `Base<Cmd>`, not a
  `Base<Login>` — and with a `vararg` parameter no argument reaches `T` at all, leaving only `C`
  bound from the receiver. The recursive bound is the map in both directions: the application of
  `Base` in the known value's hierarchy carries the answer (`Login` → `Base<Cmd>` → `T = Cmd`), which
  is what kotlinc solves. Keeping the violating binding, or leaving the variable open, drops the
  candidate as violating its own declared bounds — reported as "unresolved Java static …" or
  "argument type mismatch: … but 'Iterable<Base<T>>' was expected".

  The re-solve replaces only a binding the bounds check would have rejected anyway, and only where
  the hierarchy answers with a concrete type, so it can rescue no call that kotlinc rejects: the
  solution is always an application in the value's own hierarchy, and where that does not make the
  arguments fit, the call still fails. Explicit type arguments are never touched — a wrong one stays
  an error. Tests: `tests/bound_relation_type_variable_e2e.rs`.
- **A NULLABLE PRIMITIVE parameter is its BOX in the descriptor and its own name in `@Metadata`.**
  `Int?` compiles to `Ljava/lang/Integer;` while `@Metadata` keeps `kotlin/Int`, so metadata alignment
  has to relate the two. Comparing them through the classifier ERASURE GROUPS does not: those relate
  mapped builtins (`kotlin/List` ↔ `java/util/List`), and a box is not one — so the comparison failed,
  `meta_callable_aligns` returned `None`, and the function lost its alignment outright. Parameter
  NAMES go with it, which is why a call passing no primitive at all still reported "no parameter with
  name 'x' found" for every named argument. This is the same failure shape as the value-class
  erasure case above, one arm further along the same `else if` chain.

  The pairing is the primitive→wrapper table (`kotlin_prim_to_wrapper`), which is also the single
  source of truth for the emit-side boxing, keyed by `TypeName` so the hot alignment path stays a
  pointer compare. An unsigned type's box is its own inline-class wrapper (`kotlin/UInt`), not a
  `java/lang/*`, and the table already says so. Alignment is not only about names: it decides which
  metadata function owns a JVM descriptor, so `h(x: Int?)` and `h(x: Any?)` — `(Integer,String)` and
  `(Object,String)` — were resolved to each other's signatures, which compiles and then throws
  `ClassCastException`, or silently calls the wrong overload.
  Tests: `tests/nullable_primitive_parameter_name_e2e.rs`,
  `jvm::classpath::fq_tests::metadata_param_matching_boxes_a_nullable_primitive`.

- **A lambda packed into a `vararg` is shaped by the ELEMENT type, not the declared array.** A
  vararg parameter is declared as its array (`vararg selectors: (T) -> R` is `Array<out (T) -> R>`),
  but each argument packed into it has the element type. Shaping a lambda argument from the declared
  parameter therefore asked whether an array is a function type, which it never is, and the lambda
  was left unshaped: `it` had no type and every member read on it was "unresolved reference". The
  first argument survived by coincidence — its position matched the parameter's — so the gap
  presented as "the second lambda onwards". A spread argument (`*selectors`) IS the whole array and
  keeps the declared type. The element is taken on the lambda path alone: the ordinary argument path
  reads the same value and takes the element of a final vararg itself, so unwrapping the shared value
  double-unwraps it for every non-lambda argument, collapsing the expectation to an error type — that
  rejects `describeAll(if (c) { { it.path } } else { { it.method } })` and
  `nested(arrayOf(), arrayOf("x"))`, and drops the `Long` expectation on `longs(1, 2)` so the
  constants load as widened ints (`iconst_1; i2l`) instead of `lconst_1`.
  Tests: `tests/vararg_lambda_element_shape_e2e.rs`.
- **A callable's type variable can be bound by a LAMBDA argument's result during signature
  inference.** A member property's type comes from the signature pre-pass, which asked the resolver
  only for a call's already-substituted return. A variable reachable solely through a lambda's result
  — `fun <T, R> Iterable<T>.map(transform: (T) -> R): List<R>` — is erased to its bound by then, so
  `val items = listOf(dto).map { Item(it.id) }` typed as `List<Any>` and every member read on an
  element was "unresolved reference"; the same property written as a LOCAL val, or given an explicit
  type, was fine, because those are typed by the full checker. A lambda argument is contextual: its
  parameter types come from the callable's own symbolic parameter, and its body's type binds what
  nothing else can. This is the shaping the constructor path already did, asked of whichever callable
  the call selects — member, extension, or static are candidate kinds inside selection, never
  separate operations, so the signature is reported through one accessor. The receiver's own type
  arguments are applied by the resolver before the signature is handed over (`List<Dto>` answering a
  `fun <T> Iterable<T>.map` receiver needs the hierarchy walk), leaving the caller exactly the formals
  its arguments must bind. A labelled call declines — reordering arguments needs parameter names, and
  binding from the wrong argument is worse than not binding — and a signature whose formals are not
  all bound keeps whatever the ordinary path inferred.
  Tests: `tests/lambda_result_type_variable_e2e.rs`.
- **A checked declaration type containing `<error>` must carry a diagnostic; a cross-file source
  `typealias` resolves by Kotlin scoping, not module-wide.** Two halves of one invariant break,
  found on intellij-community's `intellij.kotlin.base.projectModel` (metadata emission panicked
  `semantic type '<error>' cannot appear in Kotlin metadata` with ZERO diagnostics — the builder's
  invariant detector, which stays). (1) Signature collection's pass-1 name table maps every
  top-level class simple name module-wide, so a declared type naming an UNIMPORTED class from
  another package resolved there while the properly scoped checker (`select_classifier`) produced
  `Ty::Error` silently; member/constructor shapes then panicked in `@Metadata` encoding and
  top-level shapes silently COMPILED against the wrong-scope resolution (kotlinc rejects both).
  `check_declaration_type` (and the property annotation/receiver channel, `type_ref_ty_reported`)
  now reports `unresolved reference` exactly like `check_type_parameter_bound` — a duplicate of a
  signature-collection report collapses in the sink. (2) That reporting exposed the true intellij
  root cause: `typealias KotlinDependencyId = Long` used from a SIBLING file. A same-file alias use
  is rewritten by the parse seam, and an alias to a CLASS answers through its classifier record,
  but a primitive-/function-type-target alias has no classifier, so a cross-file use had nothing to
  resolve through. The checker now probes the collected `source_alias_expansions` under Kotlin
  scoping — explicit import as the selected root, then the import levels (own package, star
  imports, defaults) with two distinct hits in one level ambiguous — and substitutes the use-site
  type arguments into the expansion (`scoped_source_alias_ty`); an unimported foreign-package alias
  stays unresolved. Use-site projections ride the substituted arguments through
  `projected_typeref_argument`, so `P<out CharSequence>` keeps its `+` marker in the emitted
  generic signature (kotlinc-identical) and `P<*>` keeps the same out-projected-upper-bound form
  the SAME-FILE spelling produces. Cross-file function-type-target aliases still fail in signature
  collection (pre-existing, unchanged). KNOWN DIVERGENCE (pre-existing, unchanged by this work):
  kotlinc resolves classifiers and typealiases in ONE namespace level-by-level, but krusty's
  checker exhausts every classifier channel before this alias probe runs, so a LOWER-precedence
  classifier still shadows a HIGHER-precedence alias cross-channel — `typealias Sequence = Long` in
  the file's own package loses to the default-imported `kotlin.sequences.Sequence`. Tests:
  `tests/unimported_cross_package_type_e2e.rs`, `tests/cross_file_typealias_e2e.rs`.
- **`a ?: b` types a property initializer.** Signature inference — the pass that types a property
  before full checking — had arms for `if` and `when` but none for the elvis, so the whole
  initializer inferred nothing and a property written
  `val HOST = System.getenv("APP_HOST") ?: DEFAULT_HOST`, the ordinary spelling of a configurable
  constant, could not be typed at all; every later read of it was then reported as an unresolved
  reference, which is what the gap looked like from the outside. The value is the left side when it
  is non-null and the right side otherwise, so the type is the two sides' with the LEFT side's
  nullability discharged — that is exactly what the elvis discharges, and keeping it would type the
  property nullable and reject the member reads the source makes on it. Only the left side's:
  `a ?: b` with a nullable `b` stays nullable.

  The two sides must AGREE. Kotlin's type for a mix is their least upper bound — for `Int` and
  `Double` that is `Comparable<*> & Number`, which the reference compiler emits as `Object`, never as
  a widened primitive — so reusing the arithmetic promotion that serves the `if`/`when` arms would
  type `maybeInt() ?: 2.5` as `double`, a field descriptor kotlinc never writes and a different value
  at runtime. A mix declines instead, which costs an inference on a shape that erases to `Object`
  anyway.

  A PLATFORM right side (`String!` from a Java method) keeps its flexible type rather than having its
  nullability discharged: the property would otherwise claim to be non-null, which is a guarantee the
  declaration never made, and the field carried a `@NotNull` while holding `null` at runtime.

  A right side that never yields a value (`?: throw`, the idiom for a required setting) leaves the
  left side's own type. That is read at the elvis itself rather than by giving `throw` a type:
  `Nothing` reaching the `if`, `when`, block and bare-initializer paths lets `val a = throw E()`
  infer a type and emit a `Ljava/lang/Void;` field, where kotlinc rejects the property with
  "property type 'Nothing' needs to be specified explicitly". The same holds for `return`, which does
  not belong in an initializer at all. Either side untypeable still declines: a pass that answers
  where it should decline suppresses the diagnostic that would have rejected the source, and that is
  how it and the checker come to disagree.
  Tests: `tests/elvis_signature_inference_e2e.rs`.
- **A same-module extension reports the RESULT of its call like any other origin.** Overload
  selection already found and chose a module-declared extension, but the facet carrying a call's
  result was an EMIT handle — a library callable — and a same-module extension emits through the
  module path instead, so it was dropped there by a test on the declaration's origin. Asking "what
  does this name return on this receiver" then answered nothing, and a member property initialized
  through such a call could not be typed at all ("cannot infer the type of property"); the same call
  in a local val was fine, because the full checker reaches it another way. The emit handle is
  genuinely origin-specific and stays so — it describes how the call is realized — but the result is
  the same question for every origin and is now reported alongside it, bound from the receiver and
  the arguments by the same computation the handle uses, so the two cannot drift apart. A consumer
  asking what a call RETURNS no longer branches on which provider declared the callable, which was a
  provenance test standing in for a semantic one; the emit handle itself stays origin-specific,
  because how a call is realized genuinely differs between a module and a dependency.

  The result is reported only when the call's own receiver and arguments DETERMINED it. A `vararg`,
  defaulted or context-parameter call aligns its arguments differently for the emit form, and a type
  variable left unbound there specializes to its bound — reporting that would write
  `Ljava/lang/Object;` into the field, the getter and the metadata where kotlinc writes the real
  type, which a downstream module cannot consume and no box test can catch, since the program still
  runs. Those calls keep the earlier "cannot infer the type of property" instead: refusing to answer
  is recoverable, a wrong answer is not.
  Tests: `tests/module_extension_signature_result_e2e.rs`.
- **Six byte-parity rules measured off intellij's `icons-api` module (kotlinc 2.4.10).**
  (1) Float/double constants use the short ops for the EXACT bit patterns of 0.0f/1.0f/2.0f
  (`fconst_0/1/2`) and 0.0/1.0 (`dconst_0/1`) — a bit test, so `-0.0` keeps its `ldc`/`ldc2_w`,
  mirroring what `push_int` already did for integers. (2) `infix fun` publishes `Function.flags`
  bit 9 (`IS_INFIX`) in `@Metadata` — facade and class member alike; without it a consuming module
  rejects the `a f b` call form (the flag exists nowhere else). (3) A class's `@Metadata` d2 has
  a fixed intern tail: members, nested-class names, companion, sealed subclass ids, module name,
  and the class ANNOTATION strings LAST — even though `Class.annotation` (f25) serializes before
  most of those fields. (4) A `$default` stub's one-entry LineNumberTable points at the
  DECLARATION line (`fun …`), while the real method maps to its expression body's line — the two
  differ exactly when the body starts on a later line than the signature (`fn_sig_lines` vs the
  body-attributed `fn_decl_lines`). (5) A top-level extension property's accessors carry a
  LocalVariableTable naming the receiver `$this$<property>` (plus context params and the setter's
  value parameter) — the same shape extension functions already had. (6) An interface emits its
  members in SOURCE order, a property's accessors at the property's declared position (getter
  before setter), synthesized members trailing — not functions-then-accessors.
  Residues deliberately left open: a block-bodied extension-property SETTER still misses kotlinc's
  closing-brace LineNumberTable entry, reference-receiver accessors miss the
  `Intrinsics.checkNotNullParameter` prologue (moot under `-Xno-param-assertions`), and FILE
  FACADES still group property accessors after functions (the interface rule likely extends there).
  Tests: `tests/iconsapi_byte_residue_e2e.rs`.

- **A member and an extension of the same name are chosen by the lambda's WRITTEN arity.** A Java
  method taking a functional interface offers one arity — `Map.forEach(BiConsumer)` is two
  parameters — while the Kotlin extension of the same name offers another,
  `Map<out K, V>.forEach(action: (Map.Entry<K, V>) -> Unit)`, which is one. The member's lambda
  expectation was consulted first, and when it answered, the extension was never shaped at all: a
  lambda written with ONE parameter was shaped against two, so its parameter stayed untyped and every
  member read on it was reported as an unresolved reference. A destructuring parameter is one
  parameter — `{ (key, value) -> … }` binds a single value and destructures it — which is why that
  spelling failed the same way as `{ entry -> … }` while `{ key, value -> … }` worked. An expectation
  whose parameter count cannot fit the lambda as written is not an expectation for this call, so the
  extension still gets its turn; an implicit `it` names exactly one parameter. This decides between
  candidates by what the source says, not by which provider declared them, so a Kotlin class
  extending a Java one behaves identically to the Java one.
  Tests: `tests/member_extension_lambda_arity_e2e.rs`.

- **A declaration's type does not depend on the order the compiler was asked in.** Signature
  collection types an implicitly-typed property before full checking, and it walked files in FILE
  ARGUMENT order, so a property initialized from a declaration the walk had not reached yet could
  not be typed at all. `A.kt` = `val base = listOf(1, 2, 3)`, `B.kt` = `val derived = base.map { it + 1 }`:
  `krusty A.kt B.kt` compiled and `krusty B.kt A.kt` reported "cannot infer the type of property
  'derived'", while kotlinc accepts both and emits identical bytes. Because every read of an
  untyped property is then reported as an unresolved reference, one such property produced errors
  across every file that used it, which is what the gap looked like from the outside.

  A declaration whose type the walk cannot determine is no longer an error at that point. It is
  recorded and resolved afterwards ON DEMAND: asking for a declaration's type resolves it then, and
  the answer is remembered, so the order declarations are asked for cannot change any of them. This
  is how the reference compiler is built — `ReturnTypeCalculatorWithJump` types an implicitly-typed
  declaration by jumping to it and running real body resolution, and
  `ImplicitBodyResolveComputationSession` holds exactly a memo keyed by declaration, the stack of
  declarations being computed, and the loops found (verified against the shipped
  `kotlin-compiler.jar` for 2.4.10). It replaces the retry-to-fixpoint passes that approximated
  demand ordering by sweeping the module until nothing changed.

  Termination is structural rather than a round budget: a declaration reached while it is already
  being computed is a cycle, and every declaration on that loop declines — `val a = b; val b = a`,
  `val a = a`, and a loop closed through an expression getter or across a file boundary all report
  at each declaration on the loop, as kotlinc does ("type checking has run into a recursive
  problem"). A declaration on a loop keeps the decline rather than whatever value was computed on
  top of the recursive answer; publishing that would make the type depend on which member of the
  loop was asked for first, which is the order dependence being removed. A declaration merely read
  by two others is not a loop and still resolves.

  Resolving on demand must not widen the INITIALIZATION model, which is a separate question from
  where a declaration's type comes from. An initializer runs in declaration order, so a declaration
  written later in the SAME FILE has no value yet and cannot type it — kotlinc rejects
  `val eager = later` followed by `val later = 1` with "variable 'later' must be initialized" — while
  the identical pair split across two files is accepted (both measured on 2.4.10). Same-file source
  order therefore restricts what an eager initializer may read, and only an eager initializer: an
  expression getter is an executable body and may name a declaration written later, which is why
  `val early get() = later` types the same whichever of the two is written first. Module-wide
  position comparison would be wrong, because it would reject the cross-file spelling kotlinc takes.

  Refusing to answer stays recoverable and answering wrongly does not: an inferred declaration type
  becomes the field descriptor, the getter descriptor and the `@Metadata`, so a wrong one is a
  miscompile that runs green. One place turns "no answer" into a decline, and no consumer invents a
  type when the engine gave none.
  Tests: `tests/resolution_order_independence_e2e.rs`, `tests/resolution_cycles_e2e.rs`,
  `src/type_engine.rs` unit tests.

- **A declaration a reference SHADOWS is answered by that declaration or not at all.** Resolving
  declarations on demand needs an index from a spelling to the declaration it names, and the obvious
  index — module-wide by simple name — answers references it has no business answering. Three shapes,
  each measured against kotlinc 2.4.10, each a wrong declared type rather than a diagnostic:

  A read THROUGH a receiver (`other.a`, `this.a`, an implicit companion receiver) never reaches a
  bare-name hook: it resolves against the symbol table's member records, which hold a placeholder
  while that member's own type is still being determined. Reading the placeholder as the answer
  rejected `class Box { val a = Helper.text() }` / `class User { val b = Box().a }` with `Helper` in
  another file, which kotlinc compiles. The engine fall-through therefore belongs on the member-read
  path too, not only on the bare name.

  A class body resolves type spellings against its OWN classifier names — its nested classes, its
  lexical owner's and the ones it inherits, all under their simple spellings — while the file-level
  projection registers a nested class only under its dotted declared name. Resolving a member's
  initializer against the file's names declines `class Outer { class Nested; val x = wrap(Nested()) }`,
  and where a top-level class shares the simple name it silently binds THAT one into the field
  descriptor and the `@Metadata`.

  A member index keyed by the declaring owner misses an INHERITED member, and falling through to the
  module index on that miss types the reference from an unrelated declaration: with a top-level
  `val a: String` and `open class Base { val a: Int }`, `class Derived : Base() { val b = a }` came
  out `String` where kotlinc writes `private final int b`. A name the owner or any of its supertypes
  declares shadows the module property, so it is answered from the owner chain or declined — never
  borrowed from the module.
  Tests: `tests/resolution_order_independence_e2e.rs`.

- **A call's lambda arguments take their shape from ONE decision, and the sources compete on the
  merits rather than on the order they are asked in.** A member call can shape a lambda argument from
  a selected source member, from a classpath member's expectations, or from an extension. Every call
  path wrote its own priority chain over those sources, and the chains disagreed: the safe-call path
  asked an extension before a classpath member, the explicit-receiver path asked the classpath member
  first, and the implicit-receiver path swept the whole receiver tower once per source. So
  `x.f { … }`, `x?.f { … }` and a bare `f { … }` on the same receiver could shape the same lambda from
  different callables.

  Three rules, each measured against kotlinc 2.4.10, settle it:

  A MEMBER outranks an extension, so a classpath member's expectations are asked for before any
  extension is looked up, and a SELECTED member ends the search — no extension is consulted at all.
  That is not only a question of which type a lambda parameter gets: whether lambda mutation is
  allowed asks if any applicable extension is inline, so an inline extension beside a non-inline
  member kept a captured mutable local direct for a call that does not splice.

  An expectation whose parameter count cannot fit the lambda AS WRITTEN is not an expectation for
  this call, whichever source offered it. This is the rule that lets the sources be asked in one
  order at all — a source that cannot fit does not answer, so being asked first stops deciding the
  outcome. `sizes?.forEach { (name, count) -> … }` on a `HashMap` reaches the Kotlin one-parameter
  extension past the Java two-parameter `BiConsumer` for this reason alone.

  The RECEIVER TOWER is innermost first, and each receiver is asked for a whole decision rather than
  the whole tower being swept once per source. Sweeping per source let an outer receiver's extension
  shape a lambda that an inner receiver's member should have shaped — backwards on both rules at
  once. With `class Outer { fun Outer.forEach(block: (Int) -> Unit) }` and
  `with(list) { forEach { it.length } }` on an `ArrayList<String>`, `it` is `String`; it was `Int`,
  which rejected the program.

  A receiver ANSWERS only when what it offers can shape a lambda: a shape carrying receivers or
  materialization but no parameter types ends the sweep on a receiver with nothing to give, where
  asking per source used to fall through to the next receiver.
  Tests: `tests/member_extension_lambda_arity_e2e.rs`,
  `tests/implicit_receiver_tower_lambda_shape_e2e.rs`,
  `tests/build840_mm1_safe_call_lambda_ext_e2e.rs`.

- **A lambda shape spells its parameters contexts first, then the receiver, then what the author
  wrote.** `context(P) R.(X) -> T` carries a three-entry parameter list for a lambda the author
  writes with one parameter. Any rule that measures a shape against the lambda as written, and any
  reader that recovers the value parameters from it, has to skip both prefixes: counting the receiver
  rejects every `R.(X) -> Y`, the shape most receiver DSLs have, and counting the contexts rejects
  `context(P) R.(X) -> T`. Skipping exactly one entry — which two of the three readers did — is worse
  than rejecting the shape, because it hands the lambda its own receiver as a value parameter and one
  parameter too many.
  Tests: `tests/context_function_type_e2e.rs`.
