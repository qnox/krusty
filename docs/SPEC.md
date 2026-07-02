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
  { n + 1 }`, `make(10).invoke(k)` → 11 (`::suspend_lambda_captures_enclosing_variable`). Still skipped
  (later slices): own parameters. **Internal suspension**: a lambda whose body is a single TAIL suspend
  call (`{ foo() }`, `{ suspendOnce() }`) compiles its `invokeSuspend` to a state machine with the
  lambda instance itself as the continuation — a `label` field on the class, dispatch on `this.label`:
  state 0 threads `this` (cast `Continuation`) into the callee and sets `label=1` (a classpath/sibling
  callee, resolved by its logical signature, gets its descriptor rewritten to the CPS form here), then
  returns `COROUTINE_SUSPENDED` up if the callee suspends else the value; state 1 (the async resume,
  re-entered by the callee's `resumeWith`) returns the resumed `result`. A suspending body that isn't a
  clean tail call, or that also captures, still bails. The lambda-suspension detection walks the AST for
  call names resolving to a suspend fn (same-file or, via the resolver, classpath). Proven both
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
  for data-class `copy` and instance methods. Not yet modeled (such files are skipped, never
  miscompiled): interface defaults (kotlinc routes them through `$DefaultImpls`) and >31 parameters
  (kotlinc's multi-`int` mask).
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
- Explicit builtin operator-methods on numeric primitives: `a.plus(b)` ≡ `a + b` (same promotion);
  `a.compareTo(b)` uses IEEE total order (`{Integer,Long,Float,Double}.compare`, so
  `0f.compareTo(-0f) == 1`, `Double.NaN.compareTo(x) == 1`). Kotlin routes the *infix* form
  `a rem b` to a user `operator`/`infix` extension but the *dot* form `a.rem(b)` to the builtin;
  the parser records infix-call source form so resolver/lowering keep that distinction
  (`resolver_regression_e2e::primitive_builtin_infix_extension_source_form_matters`,
  box `infixFunctionOverBuiltinMember.kt`). `mod`/`rangeTo`/`inc`/`dec` unsupported.
  The bitwise/shift members on `Int`/`Long` (`a.and(b)`/`a or b`, `a.shl(n)`/`a shr n`/`a ushr n`,
  `a.xor(b)`) lower to the `iand`/`ior`/`ixor`/`ishl`/… intrinsic; shifts take an `Int` count, the
  others the receiver's own type. `compareTo` and the arithmetic/bitwise/shift members all share
  `lower_prim_op_method`, so an (unnecessary) safe call on a non-null primitive — `a?.and(b)`,
  `a?.compareTo(b)` — compiles identically to the plain `.` form (`tests/safe_call_prim_intrinsic_e2e.rs`).
  `inv()` (zero-arg) stays a dedicated arm.
- Safe call `a?.b` / `a?.m(args)`: evaluates the receiver once into a temp, then yields the member
  access (property `GetField` / method `MethodCall`) when the temp is non-`null`, else `null` — i.e.
  `{ val t = a; if (t != null) t.b else null }`. Lowered in the front-end so every backend shares it;
  composes with Elvis (`a?.m() ?: d`). The merge of the member arm (a reference) with the `null` arm
  types the verification stack as the member's reference type (`null` is assignable to any reference),
  not `top` — emitting a `top` there is a `VerifyError: Bad type on operand stack`. Only user-defined
  member targets are resolved; safe calls on stdlib receivers (`s?.substring(1)`) need the external-call
  path and are skipped (`tests/safe_call_e2e.rs`).
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
  parenless base class (`class A : B { constructor(): super() }` — B is a concrete file class) is
  recovered post-parse. **Field-initializer default-value elision:** kotlinc omits a field initializer
  that stores the field's JVM default (`0`/`false`/`null`/`'\0'`, incl. `0.toByte()`), so a value a base
  constructor's virtual call already wrote survives; krusty does the same (test
  `secondary_ctor_noprimary_e2e`, corpus `fieldInitializerOptimization`). The delegation `<init>`
  *target signature* is read live from the (post-`value_classes`-pass) class at emit time, so the lowerer
  needs no value-class knowledge and a value-class `super(…)` argument erases correctly. Skipped (bail,
  never miscompile): a secondary with a defaulted parameter (needs the synthetic `DefaultConstructorMarker`
  overload) and an ambiguous `this(…)` target.
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
- `try { … } catch (e: E) { … }` (no `finally`): the body value (and each catch value) is stored into a
  result temp and loaded at the merge, like kotlinc. The protected region covers the body + result
  store; each catch is an exception-table handler whose StackMapTable frame has the caught exception on
  the stack and the pre-`try` locals. A diverging body/catch (`throw`/`return`) emits no dead store, and
  a fully-diverging `try` has no merge. try in a property initializer is skipped (constructor frame
  context). `throw e` → `athrow` (`tests/try_catch_e2e.rs`). A `finally` block is inlined (like kotlinc)
  at each exit: the normal fall-through, the end of each catch, and a synthetic catch-all (any
  throwable) covering the body + catch handlers that runs the `finally` then re-throws. A `try` whose
  body/catch performs a `return`/`break`/`continue` out of the `try` (which must run `finally` first) is
  skipped. **Nested `try`/`catch` is supported** (a `try` in another `try`'s body or catch — verified
  end-to-end), **except when a `finally` is involved in the nesting**: a `finally` is inlined at every
  exit of its protected region, so when it sits inside (or wraps) another `try` the duplicated code lands
  in overlapping exception ranges and trips a verify error — so a nesting that involves any `finally` is
  rejected (skip), never miscompiled (`NestedTry` in `tests/feature_box_e2e.rs`).
- `as T` to a non-null reference type throws on `null`: `Intrinsics.checkNotNull(value, "null cannot be
  cast to non-null type <kotlin-name>")` then `checkcast` — matching kotlinc. `as T?` and primitive
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
  erased element; the checker rejects a *primitive* element — `arrayOf(1, 2)` — since `Array<Int>` is
  `Integer[]` and would need per-element boxing krusty doesn't model yet). The array creators
  (`arrayOf`/`intArrayOf`/…/`IntArray(n)`/`emptyArray`) are **compiler intrinsics** — they have no
  callable body in `kotlin-stdlib` (kotlinc's backend lowers them to array bytecode by resolved symbol),
  so krusty recognizes them the same way kotlinc does: by the **resolved stdlib symbol**, gated on the
  name *not* being shadowed by a user-declared function or local (a user `fun arrayOf` wins, never the
  intrinsic) — not by bare source name. An element that lowers to a
  branch — an `if`/`when`/elvis or a **safe call** `c?.calc()` — is rejected (the file skips): a branch
  mid-`Vararg`-fill emits a StackMapTable frame inside the element-store sequence that the verifier
  rejects, so `is_branchy` treats those as non-spliceable (`ArrayOfRef` in `tests/feature_box_e2e.rs`).
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
- **`===`/`!==` on a nullable-primitive operand is rejected** (skip): boxed identity vs the unboxed
  primitive — and `Double`/`Float`'s `-0.0`/`NaN` — has subtle semantics krusty doesn't model.
- **Dead-code elimination after a diverging statement.** Statements following a `return`/`break`/
  `continue` or an expression of type `Nothing` (a `throw`, or a call that never returns) in the same
  block are unreachable; krusty drops them (and a trailing block value), matching kotlinc. Emitting them
  would leave a dead branch target without the stackmap frame the JVM verifier requires (`VerifyError:
  Expecting a stack map frame` — seen with `try { throw …; <unreachable> } catch …`).
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
- **Unsigned types `UInt`/`ULong`** — Kotlin inline classes over `Int`/`Long`; unboxed they ARE that JVM
  primitive (descriptor `I`/`J`), with unsignedness driving operation/conversion choice (kotlinc hardcodes
  these intrinsic mappings, so krusty mirrors them). Literals `1u`/`0xFFuL`; `+`/`-`/`*`/`==` use the signed
  two's-complement opcodes; `/`/`%`/`<`/`>` use `Integer.{divide,remainder,compare}Unsigned` (`Long.*` for
  `ULong`); `toString`/templates use `Integer.toUnsignedString`; `UInt.toLong()` zero-extends via
  `Integer.toUnsignedLong` (not the sign-extending `i2l`); `toInt`/`toUInt` reinterpret (no-op). Boxing into
  a reference context uses the inline-class factory `kotlin/UInt."box-impl"(I)Lkotlin/UInt;` (and
  `unbox-impl` on read, `is UInt` → `instanceof kotlin/UInt`) — never `Integer`, so identity and large
  values are preserved. `tests/unsigned_e2e.rs`. (`UByte`/`UShort`, `UIntRange` value iteration, and unsigned
  `when` subjects are not yet modeled — they cleanly skip.)
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
  `UserInline` snippet in `tests/feature_box_e2e.rs`.
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
  default-flags behavior matches kotlinc. First gated feature, `NameBasedDestructuring`: `for ([a, b] in
  e)` and `val/var [a, b] = e` are accepted ONLY when enabled, parsing identically to the `(a, b)` forms
  — both desugar to positional `component1()/component2()` calls, byte-identical to kotlinc (verified vs
  `-Xname-based-destructuring=complete`). Without the flag, `[a, b]` is rejected (kotlinc errors that the
  feature is experimental). A `var` destructured component captured and written by a closure is boxed
  into a `Ref` exactly like a plain captured `var` local (`var [a,b]=A(); val f={a=3}; f()` sees `a==3`).
  Tests: `multiDecl/*` box corpus (+96 gate), `tests/name_based_destructuring_e2e.rs`.

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
  unbounded `T` stays `Object`, a primitive bound still specializes). CLASS type parameters keep the
  erased (`Object`) model — the value-class pass owns class-bound handling. The generic `Signature`
  attribute is still omitted (same gap as above). The bound is also visible to the JVM **mapped-builtin**
  member tables below, so `x.get(i)`/`x.toInt()`/`x.length` on a `<T : CharSequence>`/`<T : Number>`
  resolve. NOT supported: a `<T : Comparable<T>>` bound whose body uses the `<`/`>` operator AND is called
  with a primitive (`maxOf2(3, 5)`) — that needs the type argument inferred (`T = Int`) and the primitive
  BOXED into the `Comparable`-erased parameter slot, which krusty's emit does not do (a raw `int` reaching
  a `Comparable` parameter is a VerifyError), so such a call is DECLINED (the file skips), never
  miscompiled. Tests: `tests/bounded_type_param_e2e.rs`.

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
    suspend detection was fixed at its root: `metadata::suspend_method_names` now reads BOTH the file
    facade's `Package.function` (field 3) AND a class/interface's `Class.function` (field 9) — it
    previously saw only top-level functions, so interface/class member `suspend` funs were invisible.
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

- **`@JvmStatic` member of a classpath `object` (`Base58Uuid.of(x)`).** kotlinc emits it as a static
  method on the object class, so it lands in the type's `companion` (static) list, NOT as an instance
  member — a call on the object value previously failed as "unresolved method on `<object>`". Both the
  checker (member-call fallthrough) and lowerer now try `resolve_companion` on the receiver's type and,
  when it matches, resolve/emit an `invokestatic` on the object class (the instance receiver is dropped,
  as kotlinc does). Test: `tests/interface_supertype_members_e2e.rs::jvmstatic_object_member`.

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
- **Named arguments to a CLASSPATH constructor (`Point(y = 2, x = 1)`).** Descriptors don't carry
  parameter names, so this needs the ctor's `@Metadata`: `metadata::class_constructor_param_names` decodes
  `Class.constructor` (field 8) → `Constructor.value_parameter` (field 2, a DIFFERENT proto shape from a
  `Function` — no name/return, value-parameters at field 2 not 6) → `ValueParameter.name`. Exposed via the
  `SymbolSource::constructor_param_names` hook; the checker's named-argument gate and the lowerer's
  classpath-`new` both reorder the labelled arguments onto positions (via `reorder_by_param_names`) before
  resolving/emitting. Test: `named_args_classpath_e2e` / `interface_supertype_members_e2e`.
- **Named args / omitted defaults on a QUALIFIED nested-class constructor (`Op.Ext(a = 1, b = "x")`,
  `Op.Ext(a = 1)`, `Op.Ext(4)`).** A qualified nested ctor's receiver names a TYPE, not a value, so the
  named-argument gate recognizes it via `qualified_nested_ctor_internal` (receiver is an out-of-scope type
  name and `Outer.Nested` resolves via `resolve_qualified_nested`) WITHOUT typing the receiver as a value
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
- **A `suspend` member's return type is recovered from its `Continuation<T>` generic argument.** The
  generic argument carries a PRIMITIVE return BOXED (generics erase primitives to wrappers), so a non-null
  primitive return unboxes to its Kotlin primitive (`java/lang/Long` → `Ty::Long` via
  `jvm_class_map::wrapper_to_kotlin_prim`), and a reference is canonicalized (`java/lang/String` →
  `kotlin/String`). Nullability applies (`ret_nullable`) only to a PRIMITIVE return — a nullable primitive
  is a distinct boxed type — while a nullable REFERENCE keeps its plain erased `Ty`, exactly as `resolve_ty`
  treats a declared `String?` (reference nullability is not carried in `Ty`), so the recovered suspend
  return matches a source-spelled reference return instead of a divergent `Ty::Nullable`. Test:
  `tests/suspend_return_type_recovery_e2e.rs`.
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
  `@Metadata` return classifier (`Classpath::metadata_member_return_class`) — which the JVM signature
  erases — so a declared `MutableList` return keeps `.add(…)`. (2) The CPS `box_returns` pass hit its
  `_ => false` fallthrough on a LAMBDA argument in `return m.map { … }`, bailing the state machine; a lambda
  argument is a value (its body is a separate impl function, not a `return` of the suspend fn) so it is now
  a leaf there (varargs recurse into their elements). Test: `tests/suspend_collection_hof_e2e.rs`.
- **A fully-qualified top-level function call `a.b.helper(args)`.** The callee is a dotted path whose prefix
  is a PACKAGE (its leftmost segment not a value in scope, via `dotted_root`) and whose last segment is a
  top-level function of that package (compiled to `a/b/<File>Kt`). The checker resolves the overload with
  `resolve_top_level_callable` and confirms the owning facade sits in the receiver's package
  (`qualified_path`); the lowerer's `lower_fq_toplevel_call` mirrors this and emits the `invokestatic` to the
  facade (a vararg/defaulted/inline FQ call bails to a later slice). Test: `tests/fq_toplevel_call_e2e.rs`.
- **A fully-qualified CONSTRUCTOR call via a package path `a.b.Ctx(x = 1, y = 2)`.** The constructor analog
  of the FQ top-level call: `a.b` is a package and `Ctx` a top-level class of it. `qualified_nested_ctor_
  internal` (checker) / `nested_ctor_internal` (lowerer) — which already resolved a two-segment nested type
  `Outer.Nested` from a bare-`Name` receiver — now also resolve a dotted-package-path receiver (`Expr::Member`)
  by joining `qualified_path(receiver)` with the name (`a/b/Ctx`), verified via `resolve_type`, then run the
  existing named/positional/reordered/omitted-default classpath-ctor resolution + `<init>$default` synthetic.
  Test: `tests/fq_ctor_call_e2e.rs`.
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
  applied only to `inline` HOFs; it now also handles a non-inline one, EXCEPT when a type parameter binds
  to a `@JvmInline value class` (user or classpath) or an unsigned type — those need UNBOXING of the erased
  parameter, not a cast, so they stay erased (`ty_is_value_class`/`value_underlying`/`is_unsigned_integer_type`
  guard). Test: `generic_fn_e2e::non_inline_generic_hof_binds_lambda_param`.

- **Java (non-Kotlin) static method calls, with overload selection (`Logf.make(x)`, `Logf.parse(s, 16)`).**
  A `.class`-read Java class's static methods land in the type's static list; the checker's class-name
  static-call path resolves the arity/type-appropriate overload via `resolve_companion` and now types the
  class-name receiver as its own `Obj(internal)` so the lowerer's classpath static-call path emits the
  `invokestatic` (previously the checker resolved it but the emit bailed). Test:
  `java_instance_e2e::calls_java_static_overloaded_methods`.

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
  `return type mismatch: expected 'String', actual 'Int'.`, while a non-return context keeps the general
  `type mismatch: inferred type is Int but String was expected`. Verified by the differential
  `diagnostics_match_kotlinc` test, which compiles each snippet with both compilers and asserts the first
  `error:` text matches exactly.

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
  first piece of real inline-class infrastructure (the mangled-name lookup); `UByte`/`UShort` and unsigned
  open-ranges/`step` are still unmodeled, so most unsigned-range corpus files (which mix all of these) stay
  skipped — but the range-value iteration itself is correct (verified past the sign bit).

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
  read on the result resolves against the raw class (element type `Any`).

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

- **Classpath dotted type references (`lib.Thing`, `Wrap.Box`) in type position.** A dotted name in a
  type position is resolved to a classpath internal by two complementary rules, tried NESTED-first (an
  in-scope type shadows a package path, as kotlinc resolves): (1) a nested type under a resolvable outer
  prefix (`Wrap.Box` → `<pkg>/Wrap$Box`), then (2) a fully-qualified package path (`lib.Thing` →
  `lib/Thing`), each verified via `resolve_type` so a bogus path stays unresolved. The signature phase
  registers such names into `class_names` (so the checker/lowerer agree), and the checker/lowerer
  `resolve_qualified_nested` share the ordering — covering both a parameter/return type ref and a
  qualified constructor call `lib.Thing(5)` (`classpath_type_ref_e2e`).

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
