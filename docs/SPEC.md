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

**kotlinc is the oracle, INCLUDING where it is wrong.** Where the reference compiler's behaviour
departs from the language specification, krusty matches the reference compiler, not the
specification. A consumer links against what kotlinc actually emits and a program runs against what
kotlinc actually does, so a krusty that were "more correct" would be less compatible — and
compatibility is the goal. The differential harness already encodes this: the expectation is
kotlinc's answer, never a reading of the spec.

Such a divergence is never followed silently. Each one is:

- **written down** as an entry in §7 saying what the specification implies, what kotlinc does
  instead, and that krusty follows kotlinc;
- **pinned by a test** that runs the shape under both compilers and asserts the EXACT answer rather
  than only that the two agree — equality alone would keep passing if krusty and kotlinc drifted
  together, and it records nothing a reader can see;
- **revisited when the reference version moves**, since a bug fixed upstream becomes a behaviour
  change krusty has to follow in the same direction.

The same rule decides an unspecified case: whatever kotlinc does is the answer, recorded the same
way, rather than a choice krusty is free to make. Kotlin's own box corpus is a lower bound on this
and not a substitute for it — it is upstream's regression suite, so a behaviour it never observes
can still be one consumers depend on (`typeMapping/nothing.kt` asks `"" is Nothing` and never reads
the result).

This is already how the §7 entries are written where the question has come up — see the cross-file
`suspend` extension entry, which follows kotlinc's emitted CPS pair and splice boundary rather than
reasoning about what an `inline suspend` declaration ought to produce. The rule above states the
practice so it is a requirement rather than a habit.

A caution that costs real time: what looks like a kotlinc divergence is usually krusty's own defect,
so establish the reference answer before concluding anything about it. `(UIntArray(1) as Any) is
IntArray` looked like a free choice — the box corpus never asks it, and Kotlin/Native answers the
opposite — but `kotlin.UIntArray` carries `box-impl`/`unbox-impl`, so the value class boxes at the
`Any` boundary and kotlinc answers `false true false` for `is IntArray`/`is UIntArray`/`is
LongArray`. krusty answers `true` there because it does not box; that is krusty's bug to fix, not a
kotlinc bug to match. Running the reference compiler is what separated the two, which is why
`docs/TEST_HARNESS.md` now insists on provisioning it.

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
  **A floating-point value renders as the SHORTEST decimal that reads back as exactly that value**,
  plainly while the magnitude is in `[10^-3, 10^7)` and as `d.dddEn` outside it — `0.1` is `"0.1"`,
  `0.1 + 0.2` is `"0.30000000000000004"`, `9999999.0` is `"9999999.0"` and `1.0E7` is `"1.0E7"`.
  Two clauses beyond "shortest" are Kotlin's own: where ONE significant digit would suffice, the
  two-digit decimals are considered alongside it and the closer to the value wins, which is why
  `Double.MIN_VALUE` is `"4.9E-324"` rather than the shorter, equally round-tripping `5E-324`; and
  `equals` on a BOXED value compares bits where `==` on two `Double`s compares numbers, so a boxed
  `NaN` equals itself and a boxed `0.0` does not equal `-0.0`, each the opposite of the unboxed
  answer. The native runtime computes the digits with exact integer arithmetic
  (`src/native/runtime/krusty_fp.c`), verified against the JVM's own rendering over a million
  values. `%` on two of them is IEEE's remainder TRUNCATED toward zero — the sign of the left
  operand and a magnitude below the right one's — which the native runtime computes exactly on the
  significands, since no instruction provides it on every target.
  Tests: `tests/float_rendering_e2e.rs`.
  `length` counts those same UTF-16 code units, which is not free for a runtime that stores UTF-8:
  the native runtime walks the bytes (`kt_string_length`), counting each byte that is not a
  continuation byte and adding one more for each four-byte sequence, because a code point above
  U+FFFF is written as a surrogate PAIR. `"aé中🙂".length` is 5 where the byte length is 10 and the
  code-point count is 4 (`tests/native_codegen_e2e.rs::a_strings_length_counts_utf16_code_units`).
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
  A LOCAL or ANONYMOUS CLASS shares the same cell the same way, through a field rather than a
  lambda capture: `var a = 1; object { init { a = 2 } }` leaves `a` at 2, because the field carries
  the cell the enclosing function allocated and not a copy of the value. Common IR keeps that
  field's SEMANTIC element type and marks the coordinate in `IrFile::shared_class_capture_fields`,
  so no backend's holder representation reaches the frontend: the JVM realizes it as `Ref$XxxRef`
  (`jvm::shared_captures`) and the native backend as a plain reference to the cell
  (`native::captures`), each at the marked coordinate and nowhere else
  (`tests/native_codegen_e2e.rs::a_local_class_shares_the_mutable_locals_it_captures`).
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
  `secondary_ctor_noprimary_e2e`, corpus `fieldInitializerOptimization`). The rule is KOTLIN's
  rather than the JVM's and holds for every target krusty emits for, because each clears an
  object's storage when it allocates (the native runtime in `kt_gc_allocate`) — so both backends
  read it from one place, `IrFile::is_elided_initializer_store`, keyed by the store's exact
  identity rather than its shape, since a later `init { x = 0 }` is a different statement with
  different meaning. The delegation `<init>`
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
- **A range's own answers, reproduced by krusty's native runtime.** The native target owns
  `IntRange`, `LongRange` and `CharRange` rather than reading them out of a library, so each answer
  is a decision written down here and pinned by `tests/native_ranges_e2e.rs`:
  - `a until b` answers the EMPTY range when `b` is the element type's minimum, rather than
    computing `b - 1` and wrapping round to the maximum — which would make every value a member.
  - An empty range is one whose `first` exceeds its `last`; that is a value, not an error, and
    `3..1` is it.
  - `equals` is true when both ranges are empty, or when both bounds match, and only WITHIN one
    range type: `1..3` is an `IntRange` and never equals the `LongRange` of the same bounds.
  - `hashCode` is `-1` for an empty range, else `31 * first + last` with each bound folded through
    its own `hashCode` first — which for a `Long` is its two halves xored together.
  - `toString` is `"$first..$last"`, with a `CharRange`'s bounds rendered as the characters they are
    (`'a'..'c'` renders `a..c`, not `97..99`).
  - Iterating a materialized range terminates at the element type's maximum: the iterator carries a
    "there is another" bit rather than testing `next <= last`, because incrementing past the maximum
    wraps and that test would never stop. Kotlin's own `IntProgressionIterator` carries the same bit.
  A `Char` bound is compared UNSIGNED, so a code point above `0x7FFF` is not a negative number:
  `'\uFF00' in '\uF000'..'\uFFFF'` is true. Tests: `tests/native_ranges_e2e.rs`.

- **An UNSIGNED `for` over a range is a counted loop on the unsigned ring, and it stops at its last
  element.** A signed `for (i in a..b)` is turned into a plain `while` by common lowering before any
  backend sees it, but an unsigned one is left as a checked range loop carrying its two BOUNDS —
  because the comparison and the step both have to be read unsigned, which is a target decision and
  not a spelling. Nothing is constructed: krusty's native target realizes it as the ordinary
  header/body/step/exit graph over one counter local, with `icmp` unsigned rather than signed.
  - Both bounds are evaluated ONCE, in source order, before the loop runs: `a..b` builds a range
    before anything walks it, so a bound with an effect has that effect exactly once even when the
    range turns out to be empty, and a body assigning to what named a bound cannot move it.
  - A CLOSED range (`a..b`, `a downTo b`) stops AT its last element: the step asks whether the
    counter has REACHED the bound and leaves the loop if it has, rather than advancing and
    comparing. `for (i in (UInt.MAX_VALUE - 2u)..UInt.MAX_VALUE)` runs three times; advancing first
    would wrap the counter to `0u` and start the walk over. `for (i in 2u downTo 0u)` is the mirror,
    and runs three times rather than stepping below `0u` round to the maximum.
  - A HALF-OPEN range (`a..<b`) needs no such guard: its header already stops one short of the
    bound, so the counter never leaves the type. `0u..<0u` runs zero times, as does `5u..1u`.
  - The step advances at the `continue` target, so a `continue` that skips the rest of the body
    still advances the counter, and a labelled `break` leaves every loop up to the one it names.
  Tests: `tests/native_ranges_e2e.rs`.

- **A string is indexed by UTF-16 code unit, whatever it is stored as.** krusty's native runtime
  stores text as UTF-8, and Kotlin's `String` is a sequence of UTF-16 code units, so `s.length`,
  `s[i]` and `s.indices` all answer in units and not in bytes or code points. A character outside
  the BMP is one UTF-8 sequence and TWO Kotlin indices, and reads back as the surrogate pair Kotlin
  stores: in `"a\u00E9\u4E2D\uD83D\uDE00z"`, `length` is 6, `s[3]` is `\uD83D` and `s[4]` is
  `\uDE00`. `x.indices` is `0..size - 1`, which for an empty receiver is the empty range `0..-1`.
  Test: `tests/native_strings_e2e.rs`, `tests/native_ranges_e2e.rs`.

- **A callable reference compares by the declaration it names, and by the receiver it bound.**
  `::foo == ::foo` is true although each `::foo` is written in its own place, and
  `one::value == other::value` is false for two distinct receivers of the same property. krusty's
  native target reaches both without any reflection metadata: it emits one TYPE per referenced
  property rather than one per site, so the type IS the declaration, and `equals` compares the type
  pointer plus — for a bound reference — the two receivers through `Any.equals`. A site that binds
  no receiver has one instance for the whole program, so its references are the same object.
  `KCallable.name` answers the property's Kotlin name; `get` answers a reference, so a primitive
  property boxes on the way out and is unboxed back at the call site.
  Tests: `tests/native_property_reference_e2e.rs`.

- **A reflective reference to a DEPENDENCY declaration is the adapter it already is.** Common
  lowering builds `"KOTLIN"::get` or `String::plus` into an ordinary function value — a synthesized
  static method calling the dependency, plus whatever receiver the site bound — whenever the site's
  type is a plain `FunctionN`, and leaves it CHECKED when the type is a reflective `KFunctionN`, so
  each target may choose its own reflection representation. krusty's native target chooses the same
  adapter, with the declaration's provider-owned identity kept beside it, so `Boolean::not ==
  Boolean::not` is true and `"O"::plus` is equal to neither. The adapter reaches the dependency
  through the ordinary dependency-call path, which is what gives the reference every intrinsic,
  value class and runtime entry point that path knows; a bound site evaluates its receiver once,
  where it is written. An ADAPTED reference — one that reorders, defaults or vararg-packs its
  operands — is not built this way and declines, since the packing is argument normalization the
  target does not own. Test: `tests/native_dependency_reference_e2e.rs`.

- **A member a source FORM is spelled as means the same thing asked by name.** `a[i]` and `!b`
  reach krusty's native generator as compiler-supplied operations, because the frontend recognizes
  the form and names one; `(IntArray::get)(a, i)` and `(Boolean::not)(b)` name the very same
  declarations through an ordinary dependency call. A primitive has no methods to reach, so the
  member path would find no entry point and decline — the declaration is what says what the call
  means, not the spelling, so the native target answers both with the operation. An array's
  indexed access is chosen by the RECEIVER rather than by the owner, since the eight primitive
  arrays and `Array<T>` are nine owners naming one operation. Test:
  `tests/native_dependency_reference_e2e.rs`
  (`a_reference_to_a_member_a_source_form_spells_calls_it`,
  `a_reference_to_an_arrays_indexed_access_reads_it`).

- **A reference to a DEPENDENCY property is an object over synthesized accessors.** A reference to
  a property this file does not declare — `String::length`, `"KOTLIN"::length` — is left CHECKED by
  common lowering, because there is no declaration here whose storage or accessors the object could
  point at. krusty's native target supplies the pair: a synthesized static getter that performs the
  ordinary dependency-property read, and, for a `var`, a setter that performs the write. What is
  built over them is the very object a reference to a property of this file becomes — `get`, `set`
  and `name` in its table, the bound receiver in its one field, one type per property and
  boundness — so two references to one declaration are equal and a bound one evaluates its receiver
  once, where it is written. `KCallable.name` answers the property's Kotlin name as the provider
  spells it, and a property the provider cannot name gets no object rather than one answering an
  invented spelling. A MEMBER EXTENSION property declines: its accessor wants two receivers where
  the object has room for one. Test: `tests/native_dependency_property_reference_e2e.rs`.

- **Text's `length` is one question wherever it is asked.** Written in source, `s.length` is a
  compiler-supplied operation the frontend names, so it never reaches a dependency-member path at
  all. Two other spellings do: a receiver typed `CharSequence`, because the property belongs to the
  interface, and the read krusty's native target synthesizes for `String::length`, which is an
  accessor call like any other. All three ask the same thing, so one runtime function answers all
  three — it reaches a string's own bytes, a builder's, and, through the descriptor, text the
  PROGRAM declared. Test: `tests/native_dependency_property_reference_e2e.rs`
  (`a_dependency_property_reference_answers_its_name`), `tests/native_walkable_text_e2e.rs`.

- **A class implements an interface member it never registers an override for.** Two shapes leave
  an interface's dispatch number empty although the method filling it is already in the class's own
  table. A MEMBER EXTENSION's override — `override fun Int.foo()` of an `interface Base { fun
  Int.foo(): String }`, and the accessor of `override val Int.a` — is recorded in no override
  table, because the frontend keeps none for one. And a member an `Interface by delegate` clause
  supplies is recorded against the one interface it forwards to, so `class Z(val a: A) : A by a, B`
  leaves `B.foo` empty although the delegation answers it: in Kotlin one member overrides every
  inherited one it matches. krusty's native target reads the number by signature in both cases,
  which is the rule it already uses up the superclass chain — a class implementing an interface
  must implement its members, and Kotlin rejects a fresh redeclaration of an inherited one, so a
  method matching by name and machine signature IS that implementation. The match must be UNIQUE:
  two members whose Kotlin signatures differ can share a machine one, since `String` and `Any` are
  both references, and the target declines rather than choose. Test:
  `tests/native_interface_implementations_e2e.rs`.

- **An `is` against a FUNCTION TYPE asks about a marker, not about the object's own type.** krusty's
  native target emits one descriptor per lambda and per callable reference, so the type written at
  `f is Function0<*>` is never the type the object wears and cannot be compared against it. What the
  two share is a runtime marker: `kotlin.Function` and one per arity, none of which has instances of
  its own, exactly as `Number` and `Comparable` already do for the value types. Every function
  value's descriptor names its arity's marker and the bare `Function` beside it — both, because
  `KType.interfaces` is flattened and an interface's own bases are not walked. Arity is what
  separates them, so `{ x: Int -> x } is Function0<*>` is false while `is Function1<*, *>` and
  `is Function<*>` are true, and a callable reference answers the same way a lambda does. 22 is
  Kotlin's largest function arity, so the set is complete. Test:
  `tests/native_function_type_checks_e2e.rs`.

- **An `is` against a REFLECTION type asks about markers too, and arity counts the receivers.** A
  property reference is an object of a type of its own — one descriptor per property — so
  `p is KProperty0<*>` compares against runtime markers rather than against the object's own type,
  exactly as a function type does. Kotlin's hierarchy is `KCallable` → `KProperty` → `KPropertyN`,
  with `KMutableProperty` and `KMutablePropertyN` beside them for a `var`, and each reference's
  descriptor names the whole flattened chain because an interface's own bases are not walked. The N
  is how many receivers `get` takes: a BOUND reference carries its receiver in the object and takes
  none, so `Holder(1)::value` is a `KProperty0` while `Holder::value` is a `KProperty1`, and a
  top-level property has no receiver either way. A reference to a `val` wears no mutable marker.
  Test: `tests/native_reflection_type_checks_e2e.rs`.

- **A declaration with a REIFIED type parameter keeps its symbol, and traps where its body cannot
  be lowered.** Kotlin permits a reified type parameter only on an `inline` function, and such a
  function is spliced at every call site precisely so that `is T` and `T::class` have a type to
  name. Its own body is therefore never the one that runs, and compiling it would have to answer
  what `T` is where nothing has said — which is what made `inline fun <reified T> Any?.isTOrNull()
  = this is T?` decline on a type parameter that had reached the generator unsubstituted. krusty's
  native target lowers the body FIRST and keeps it when it lowers — `inline fun <reified T, U>
  keep(value: U): U = value` names `T` nowhere — and puts a trap there only when it does not.
  A trap rather than NO symbol, which is what this rule first said: a reified MEMBER holds a vtable
  slot, and a table has no way to decline one entry the way a call site does, so leaving the symbol
  out left the slot naming nothing at all. The trap is unreachable by Kotlin's own rule — an
  `inline` member may not be `open` — and says so loudly rather than jumping at a body that could
  not be right. Test: `tests/native_reified_declarations_e2e.rs`.

- **A nested class's `simpleName` is the segment after what encloses it.** A package is spelled
  with dots and NESTING with `$` — `A$Companion` is the companion of `A` — so a simple name read by
  splitting on dots alone answered the whole nested spelling. Both separators count, which is what
  makes `A.Companion::class.simpleName` answer `Companion`, a named companion answer its own name,
  and a plain nested class answer its own. Test: `tests/native_reified_declarations_e2e.rs`.

- **A range operand may TRANSFER CONTROL instead of answering.** `break`, `continue`, `return` and
  `throw` are expressions of type `Nothing`, and Kotlin admits one wherever a value is expected —
  `for (j in break downTo 1u)` is a loop whose bound leaves the enclosing loop before the inner one
  is ever built. Evaluating such an operand ends the lowering: what follows is unreachable, so
  there is nothing left to emit and nothing to decline. krusty's native target reads the answer as
  "the lowering left" rather than as "no value", which is what it used to report — a bound that
  yields no value while control STAYS is the only shape it cannot lower. The rule holds for both
  bounds of a counted loop and for all three operands of a membership test. Test:
  `tests/native_range_bound_control_e2e.rs`.

- **`===` between floating-point values is the machine's comparison, not a bit comparison.**
  Kotlin's identity equality between primitives is its `==` — that is the whole of what the
  deprecation warning on the form says about it — so for `Float` and `Double` it carries IEEE's
  answers with it: `NaN === NaN` is FALSE and `-0.0 === 0.0` is TRUE, both confirmed against
  kotlinc. Comparing the bits would answer the opposite of each, and boxing the two sides to
  compare addresses would answer that no two values are ever identical. Test:
  `tests/native_float_identity_e2e.rs`.

- **`Pair(a, b)` is the pair `a to b` already builds.** No file declares `kotlin.Pair`; krusty's
  native runtime owns it, and one is built for every `a to b` and for each step of a `withIndex`
  walk. The written constructor reaches that same object rather than a second shape of it, which is
  what lets a pair built either way be equal to, read like and destructured like the other. Both
  operands cross as references, which is what a pair's fields hold. Test:
  `tests/native_pair_construction_e2e.rs`.

- **`x is List<*>` asks about a marker, and declines where the file declares a list of its own.**
  krusty's native runtime builds two kinds of list — the immutable one `listOf` answers and the
  growable one `ArrayList()` answers — and a check writes the INTERFACE, which is neither of those
  types. Both name a marker with no instances of its own, and the check compares against that, as a
  function type and a reflection type already do. The predicate is NARROWER than the one a call
  uses: a call may treat `Collection` as a list because every question a list answers a collection
  answers the same way, while a check may not, since a set is a `Collection` and wears no list
  marker. And a file that declares a collection of its own keeps declining rather than answering
  `false` for an object that is one, since the program's class wears no marker either.
  It is narrower in the other direction too, and for the mirror reason: `MutableList` and
  `ArrayList` decline, because BOTH kinds of list wear this marker — the immutable one included —
  so `listOf(1) is MutableList<*>` would have answered `true` where Kotlin/Native answers false.
  The exact spelling matters for a second reason: the descriptor a check compares against is the one
  a CLASS LITERAL reads a name off, and this one is named `kotlin.collections.List`. While the
  marker answered for `ArrayList` as well, `java.util.ArrayList::class.simpleName` said `List`.
  Test: `tests/native_list_type_checks_e2e.rs`.

- **`::prop.isInitialized` reads the field RAW, and the comparison is a node of its own.** It is
  the one read of a `lateinit` field that must not carry the throw-if-null guard every other read
  carries: the guard answers this question by throwing, and this answers it with a `Boolean`. Null
  IS the evidence — which is why `lateinit` is only allowed on a type with a null to be
  distinguished by. What the node yields is the FIELD, not the answer: common lowering wraps it in
  the ordinary null comparison, so a backend building a `Boolean` here would have it compared
  against null a second time and report every property initialized. kotlinc emits the same shape:
  no reflection and no `KProperty` value, whatever the `::prop` spelling suggests. Test:
  `tests/native_lateinit_initialized_e2e.rs`.

- **A VIRTUAL call yields what its SLOT carries, not what the receiver's class narrows it to.** The
  member a virtual call dispatches through declares the type, and for a generic member that is a
  type parameter — so the value crossing the slot is a reference even where both ends of the
  program say `Int`. krusty's native target types such a call by that declared return, which is
  what lets a site wanting a machine value unbox it; leaving the call untyped made
  `class A(a: Tr<Int>) : Tr<Int> by a`'s `a.prop` arrive in an `Int` position as a reference with
  nothing to convert it by. Test: `tests/native_virtual_call_result_e2e.rs`.

- **A SECONDARY constructor's defaults are its own, and the wrapper filling them names which
  constructor it fills.** A primary constructor's defaults live on the CLASS, beside its
  parameters; a secondary's live on the constructor. krusty's native target fills both the same
  way — a wrapper takes the operands actually supplied, evaluates each missing default into the
  frame in declaration order so a later one may read an earlier parameter, then runs the
  constructor — but what the wrapper is keyed on has to name WHICH constructor it fills, since two
  constructors of one class omitting the same ordinal are two frames and two wrappers. The same
  key answers a `super(…)` delegation: a base whose only constructor is a secondary is reached
  that way, and reading the primary's frame for it would fill a frame that does not exist. Test:
  `tests/native_secondary_constructor_defaults_e2e.rs`.

- **A data class renders an ARRAY property's contents, and compares it by identity.** `data class
  D(val xs: IntArray)` prints `D(xs=[1, 2])`: the generated `toString` shows what an array holds
  rather than the identity the array's own `toString` answers, which is the same rendering
  `xs.contentToString()` asks for and reaches the same runtime entry point. Only `toString` is
  special this way — `equals` and `hashCode` on such a property stay the array's own, so two data
  classes holding equal contents are UNEQUAL. That is Kotlin's rule rather than an omission, and
  krusty's native target keeps the two apart. Test:
  `tests/native_data_class_array_rendering_e2e.rs`.

- **A `ReadOnlyProperty` delegate is dispatched among the file's own classes.** `val x by
  Delegate()` reads through `kotlin.properties.ReadOnlyProperty.getValue`, a dependency member, and
  a call through a dependency interface cannot go through a slot: an override of one takes a slot
  of its own, so the interface names no number to dispatch on. What makes the choice possible here
  is that krusty's native runtime builds no `ReadOnlyProperty` at all — unlike `ReadWriteProperty`,
  whose `notNull()` and `observable(…)` it does build — so every object that can stand behind the
  type is a class of the file, and testing the receiver against each is exhaustive. The last arm is
  still emitted and fails loudly, for an object that cannot arrive. Test:
  `tests/native_read_only_property_delegate_e2e.rs`.

- **An array's emptiness is its length, and `toTypedArray` copies what the receiver walks.**
  `xs.isEmpty()` and `xs.isNotEmpty()` on an array ask one question of the length, and it is the
  same question whichever element width the array has — a `DoubleArray` answers it as an
  `Array<String>` does. `xs.toTypedArray()` answers a reference `Array<T>` holding what the
  receiver walks: a COPY, so writing through the array does not reach the collection it came from,
  and a copy rather than a conversion of each element, since a collection's elements are already
  references. Test: `tests/native_array_emptiness_e2e.rs`.

- **A `Throwable` carries a CAUSE, and the two one-argument constructors are told apart by type.**
  Kotlin declares four: `()`, `(message)`, `(cause)` and `(message, cause)`. A single argument whose
  type is a `Throwable` is the cause and anything else is the message, and the two forms differ in
  more than which field they fill — `Throwable(cause)` takes its MESSAGE from the cause as well, as
  `cause?.toString()` — so neither can be rewritten as the other. krusty's native runtime holds both
  fields, and the collector traces each. A class of the PROGRAM extending `Throwable` is laid out on
  top of that storage, so its constructors write the two fields in place where the base's
  constructor would have run — the primary and the secondary paths alike, all four forms reaching
  the same pair. A `null` cause is written rather than left alone: an unwritten field is whatever
  the allocation left there, and this one is traced. `e.cause` reads it through the runtime, as
  `e.message` already did, because the class is the runtime's and so is its layout. Both are
  `open val`s, though, and reading one through the runtime answers the FIELD — which is right for
  every throwable the runtime makes and wrong the moment a class of the program redeclares one,
  since Kotlin dispatches to the override and the base reserves no slot to dispatch through. A file
  that overrides either therefore declines the read rather than answering the wrong half of the
  question. Test: `tests/native_throwable_cause_e2e.rs`.

- **The runtime walks a collection the PROGRAM declared, through three thunks its descriptor
  carries.** Every walking entry point of krusty's native runtime — `withIndex`, `contains`,
  `count`, `map`, `joinToString`, the `for` loop itself — reaches its elements through `iterator()`,
  `hasNext()` and `next()`, and those knew only the shapes the runtime builds. A class of the
  program puts its own members at a vtable slot assigned per program, which the runtime cannot
  guess, so such a receiver declined. Its descriptor now carries a small emitted thunk for each of
  the three, with a fixed signature this side can call — a POINTER rather than a slot number,
  because an emitted method has the signature its declaration states and neither `hasNext`'s
  machine `Boolean` nor an `Iterator<Int>`'s unboxed element could be read from a number. Each
  thunk dispatches VIRTUALLY, so one serves the whole subtree below the class that declares the
  member.
  WHICH members to look up follows from the ROLE the class answers for, not from the override
  edges one by one: an override whose result is the interface's own type parameter records no edge
  of its own — `Iterator<T>.next(): T` is exactly that shape — and half a pair is no walk. A class
  answering for `Iterable` is walkable when its `iterator` resolves; one answering for `Iterator`
  when BOTH of its two do.
  TEXT is walked the same way, through two more thunks. `String`, `CharSequence` and
  `StringBuilder` are a SHAPE OF THEIR OWN: a `for` loop walks them as it walks a list, but their
  own members are `length` and the indexed read rather than an iterator — Kotlin's `CharSequence`
  declares no `iterator` at all. `kt_string_length` and `kt_string_get` reach those two for an
  object that is neither a string nor a builder, so `s[i]` through a `CharSequence` receiver, a
  `for` loop over one and `withIndex` on one all answer for text the program wrote; the chars
  iterator the runtime already had needs no case of its own. Both or neither, as with the
  iterator's pair.
  A `Sequence` of the program's is one of these classes: its one member IS `iterator`, so the walk
  and the thunk are the same. What the shape adds is the narrowing above — the class is recorded as
  a sequence as well as as a walkable, and a receiver typed by it takes only the members that are
  lazy either way.
  What still declines is a shape the walk does not reach: `Map` and `Map.Entry` declare none of
  these members, so a file that puts a class of its own behind one of them declines every walking
  member over that shape, since no static type tells one implementor from another within it. A
  member Kotlin gives a SPECIAL BRIDGE — `Collection.contains`, `List.indexOf`/`lastIndexOf` — is
  never a walk's to answer where the file implements the receiver's type: the bridge decides by
  whether the argument can be what the declaration accepts, and a walk would compare elements
  instead (`codegen/box/bridges/strListContains.kt`).
  Tests: `tests/native_walkable_collection_e2e.rs`, `tests/native_walkable_text_e2e.rs`,
  `tests/native_collection_shape_guard_e2e.rs`.

- **A delegated property's `KProperty` metadata is a property reference, and a LOCAL one answers
  only its name.** `val x: Int by D()` hands the delegate an object so `getValue(thisRef, property)`
  can ask the property about itself; for a member property that object is exactly the reference
  `C::x` is, and krusty's native target emits one and the same type for both. A LOCAL delegated
  property has no storage and no accessors to reach, and Kotlin gives its metadata no receiver to
  read through, so `get` and `set` on it are the runtime's loud failure rather than a body — no type
  a program can name there declares them.
  The metadata a member's delegation needs is a static the class owns, and on the JVM an owner says
  WHEN the initializer runs. Here it says nothing, because this initializer cannot tell: a property
  reference with no bound receiver is one object per property with no state and nothing to allocate,
  the same value however early it is asked for — which is the same reason a `const val`'s owner says
  nothing. Any other class-owned initializer still declines rather than guess at the order.
  Tests: `tests/native_property_reference_e2e.rs`.

- **A property reference carries at most ONE receiver, and which kind it is does not change the
  object.** `x::p` binds a class's receiver and `"ab"::ext` an extension one; either way it is the
  single operand the accessor leads with, so krusty's native target stores one field and `get` takes
  one argument. `String::id` on an extension property is the unbound form of the same thing, and
  calls the top-level accessor the checked lowering built rather than reading storage — an extension
  property has no object of its own to keep a field in. A property whose accessor wants MORE than
  one receiver (a member extension property) or operands beyond it (context parameters) is declined:
  the object has no room to carry them. Which it is comes from the property's LAYOUT rather than from
  the reference node's `extension_receiver` flag, which is a fact about the accessor's parameter list
  and not about this object.
  Tests: `tests/native_property_reference_e2e.rs`.

- **A CONTEXT PARAMETER of a `fun interface` method is a leading value parameter, and a SAM
  conversion needs nothing for it.** `context(c: C) fun foo(x: Int): Int` records `c` as the first
  entry of the method's own parameter list — that is how the callable header keeps a context
  parameter — so the object a SAM conversion builds carries the same captures it always did and its
  thunk wears the interface member's own signature. Every operand is handed on POSITIONALLY, which
  is the treatment an extension receiver already got: neither is a shape the thunk has to know
  about, because both arrive in the order the declaration states. krusty's native target declined
  the conversion whenever the method declared one, which was a guess about a difference that is not
  there; the ordering is pinned by a two-parameter case rather than inferred from a single one.
  Nothing here is a claim about the CONTEXT's own resolution, which is the frontend's: what this
  says is only that the backend has no per-shape work left once the header has ordered the
  parameters.
  Tests: `tests/native_context_parameter_sam_e2e.rs`.

- **A class implementing a FUNCTION TYPE puts its `invoke` at the fixed function number, or a
  converting stand-in there.** A function value's body sits right after `kotlin.Any`'s three and
  the runtime names that number itself (`KT_SLOT_INVOKE`), so every caller through a function type
  READS it rather than asking — passing and reading references, which is the one signature every
  function value shares. A class whose `invoke` carries references throughout stands there itself.
  `class A : (Int) -> Int` does not: a caller would read a machine integer as a pointer. That one
  takes a slot of its own and the fixed number holds a stand-in that unboxes each operand, forwards
  and boxes the answer — the same shape the uniform lambda entry point already had, with the
  difference that the signature it wears belongs to `kotlin.Function{N}`, which is declared in no
  file this target compiles, so it is written out from the ARITY rather than read off a
  declaration. It forwards by DISPATCH, so a subclass's override is reached through the same
  number.
  The table has to be GROWN to reach that number: `kotlin.Any`'s three are all a class starts with,
  and the fourth entry is the one wanted. Growing it before the method takes a slot of its own is
  also what keeps the two numbers apart, since the method would otherwise be pushed at exactly the
  index the stand-in wants and a stand-in forwarding through its own number dispatches to itself.
  What DECLINES is a class implementing more than one function type: `object Test : () -> Unit,
  (Boolean) -> Unit` declares two bodies that both belong at one number, and whichever took it, a
  call through the other type would reach the wrong one — which is what
  `codegen/box/funInterface/intersectionTypeToFunInterfaceConversion.kt` showed by answering `KK`
  for `OK`. Declining beats picking.
  Tests: `tests/native_function_slot_bridge_e2e.rs`.

- **A list is the array a vararg call already built, with a header.** `listOf(...)` reaches a
  backend with its elements packed into an `Array<T>`, and krusty's native target wraps that array
  rather than copying it — which is what Kotlin's own `listOf(vararg)` does, and what makes the
  elements traced by a collector that already knows how to trace an array. Being READ-ONLY is what
  makes sharing sound: nothing a program can write through reaches it. `MutableList`, `Set` and
  `Map` are not this type and decline by name. The list answers all three of `kotlin.Any`'s members
  by its contents, as Kotlin's `List` does.
  Which call reaches the runtime is decided by the RECEIVER's type, never by the owner declaring the
  member — the same rule ranges follow, and for the same reason: `iterator` is declared on
  `Iterable`, which a user class may implement, and a file declaring such a class overrides a
  dependency method and is declined whole.
  **`Iterable` is the one type that cannot decide, because a RANGE is one too.** A generic body or
  an inlined stdlib extension — `for (e in this)` inside `Iterable<T>.forEach` — types its receiver
  by the interface, and no static type can then say which of the two iterable things the runtime has
  is in hand; reading a range as a list takes a bound for a pointer. So an interface-typed receiver
  routes to a runtime dispatch on the DESCRIPTOR, and `next` there answers a reference, because a
  receiver typed by the interface has its element type erased.
  Tests: `tests/native_lists_e2e.rs`.

- **Which `listOf` a call selected is a fact about the declaration, and only its PHYSICAL parameter
  says so.** Kotlin declares `listOf` twice — over a vararg and over one element — and which one a
  call took decides whether its argument IS the list's elements or is one OF them. Neither the
  argument nor the semantic parameter can answer: the single-element overload's parameter is `T`,
  and `T` may itself be an array, so `listOf(anArray)` takes that overload and answers a list of one
  array; and `arrayOf(1, 2, 3)` lowers to the very vararg node a packed call would have, so the two
  arrive looking identical. A vararg parameter is PHYSICALLY an array however its element type was
  substituted, which is the one thing that separates them.
  Tests: `tests/native_lists_e2e.rs`
  (`a_single_element_list_is_not_a_vararg_call_of_length_one`).

- **An array and a string are walkable even though neither is an `Iterable`.** Kotlin declares
  every `Iterable` member for both, through extensions, and a program may keep the receiver as a
  value and ask it for an iterator. So the runtime walks both, and what decides how an element is
  read is the receiver's own DESCRIPTOR: an array's says how wide the element is and how to box its
  bits, which a receiver typed by an interface cannot. A string's "elements" are the UTF-16 units
  Kotlin counts, so its walk is the one `length` and `get` already pay for rather than a pass over
  the bytes.
  Giving them an iterator is not about one member: every `Iterable` member reaches them through the
  same dispatch once they have one, so `joinToString`, `map` and `forEach` came with it.
  A primitive array's iterator is also a concrete stdlib CLASS — `ByteArray.iterator()` is a
  `ByteIterator`, which declares `nextByte` beside the inherited `next`. Every spelling reaches the
  same object; what differs is the type the call site expects, and the runtime having boxed by the
  array's own element descriptor is what makes the unboxing read the bits that were written.
  Naming those concrete classes is safe for the reason the interfaces are: the only objects wearing
  one here are the runtime's own walks, and a file declaring its own subclass of one overrides a
  dependency method and is declined whole. `IntIterator`, `LongIterator` and `CharIterator` are
  deliberately absent from that list — a range's iterator wears those, and the narrow protocol below
  reads them first.

  A primitive array's iterator answers through TWO protocols. `IntArray.iterator()` has the static
  type `IntIterator`, whose `next` carries a number rather than a reference — the same narrow
  protocol a range's iterator uses — so the walk answers there as well, and the receiver's own
  descriptor is what tells the two objects apart at run time. A reference array's iterator is typed
  `Iterator<T>` and never reaches that path, which is why a pointer is never returned as a number.
  Tests: `tests/native_lists_e2e.rs` (`an_array_is_walked_at_its_elements_own_width`,
  `a_string_is_walked_by_utf16_unit`, `an_arrays_iterable_members_reach_the_same_runtime_walk`,
  `an_arrays_iterator_answers_through_the_narrow_protocol_too`,
  `a_primitive_arrays_iterator_answers_its_own_narrow_spellings`,
  `every_primitive_arrays_iterator_answers_at_its_own_width`).

- **`withIndex()` is lazy, and yields a data class.** It answers an ITERABLE rather than a list:
  Kotlin's is lazy, and the loop consuming it may stop early. The object it makes keeps the source
  until something asks it for an iterator, and that iterator counts as it walks — so the index is a
  position in the walk and not a property of what is being walked, which is what makes
  `(10..12).withIndex()` count from zero. `IndexedValue` is a Kotlin data class, so its `equals`,
  `hashCode` and `toString` are the data class's and not identity's, and `component1`/`component2`
  answer the same two questions a destructuring asks.
  Tests: `tests/native_lists_e2e.rs` (`with_index_pairs_each_element_with_its_position`,
  `a_with_index_walk_destructures_and_stops_where_it_is_told`,
  `a_range_walks_with_an_index_the_same_way_a_list_does`, `an_indexed_value_is_a_data_class`).

- **A floating-point field compares and hashes by its BITS.** Kotlin's `Double.equals` is not
  `==`, and it disagrees with it in both directions: `NaN` equals itself, and `0.0` does not equal
  `-0.0`. A data class holding one therefore reinterprets the value as an integer of the same width
  and compares that — a reinterpretation and never a conversion, since converting would round and
  rounding `NaN` loses the distinction the rule exists for. The hash comes off the same bits, which
  is what makes `NaN`'s hash a number at all: `Float` gives its 32 directly and `Double` folds its
  64 exactly as `Long` does. Both answers were pinned against krusty's JVM backend.
  Test: `tests/native_codegen_e2e.rs` (`a_floating_point_data_class_field_compares_by_its_bits`).

- **An override that changes representation is reached through a BRIDGE in the base's slot.**
  `A<T : Number>.foo(): T` erases its result to a reference, and `Z : A<Int>` overriding it returns
  an unboxed machine integer. The base's slot cannot hold that body — a caller reading the slot
  through `A` would read an integer as a pointer — so it holds a small function with the BASE's
  signature that converts each operand and forwards. The override keeps a slot of its own, with its
  own signature, which is what a call through `Z` reads.
  The bridge forwards by DISPATCH rather than by calling the override, and that is the whole reason
  it names a slot and not a function: a further subclass replaces the target slot with its own body,
  and a bridge that had named the override would keep running the wrong one.
  An INTERFACE base of a different representation still declines. Its slot number is placed
  program-wide rather than in this class's vtable, so there is no entry here to put a bridge in.
  Tests: `tests/native_classes_e2e.rs`
  (`an_override_that_changes_representation_is_reached_through_a_bridge`,
  `a_bridge_reaches_a_further_override_rather_than_the_one_that_needed_it`,
  `a_bridge_converts_its_arguments_as_well_as_its_answer`).

- **`super.p` on a property is the base class's own realization, reached without dispatch.** A
  `super` access on a PROPERTY names the property and not an accessor, and a class whose accessors
  are the default ones declares no method for it at all — so the method search the native generator
  does for `super.f()` finds nothing to call. What the program asked for is still well defined: the
  NAMED class's implementation. A source-written accessor of that class is called directly; a
  default one is the field that class contributes, which is a distinct field from the override's
  because an overriding `var` declares storage of its own — and `super.b` is exactly how a program
  can tell the two apart.
  Nothing on this path may dispatch. `super.p` is written inside the override of `p`, so reaching
  the slot would reach the accessor doing the asking and the program would not finish.
  Tests: `tests/native_classes_e2e.rs`
  (`a_super_property_read_reaches_the_base_classs_own_storage`,
  `a_super_property_write_reaches_the_base_classs_own_storage`,
  `a_super_property_reaches_a_written_accessor_without_dispatching`,
  `a_super_property_of_an_interface_reaches_its_default_accessor`).

- **A class literal is one type descriptor, and `KClass` is equal by the type it stands for.**
  `String::class` names a type statically; `x::class` reads the descriptor the OBJECT is wearing, so
  `val x: CharSequence = ""` answers `String::class`. A scalar receiver is boxed first — there is no
  descriptor on a machine integer — which is also why `(n++)::class` answers `Int::class` and not
  the class of something that has no object; the receiver still runs, exactly once.
  The object is an ordinary allocation rather than a canonical instance, because Kotlin promises
  `KClass` equality by the class and not identity: the runtime compares descriptors, so
  `x::class == String::class` is true without a table of canonical instances existing anywhere.
  `simpleName` and `qualifiedName` come off the descriptor's own Kotlin name, which every type
  already carries for `toString`. `KClass.toString` prints `class <qualified name>` — what a JVM
  WITH `kotlin-reflect` prints; a JVM without it appends "(Kotlin reflection is not available)",
  which is a fact about that dependency rather than about either backend, so the cross-backend
  differential covers the two names and not the rendering.
  Tests: `tests/native_class_literals_e2e.rs` (`a_bound_literal_answers_the_runtime_class`,
  `two_literals_of_one_type_are_equal_without_being_the_same_object`,
  `a_bound_literal_evaluates_its_receiver_exactly_once`,
  `a_literal_over_a_primitive_names_the_boxed_type`, `the_names_agree_with_the_jvm_backend`).

- **`throw` on the native target reports the exception and ends the program — and that is Kotlin's
  answer, not a stopgap.** A file containing any `try` is declined WHOLE, so inside a file this
  backend emits there is no handler and no `finally` between a throw and the end of the program. An
  exception nothing handles ends the program in Kotlin, so terminating at the throw is the correct
  behaviour for every program this backend accepts today. It exits 134, the code the JVM backend
  uses for an abnormal end and the one a failed cast already uses here, and writes
  `Exception in thread "main" ` followed by the exception's `toString`.
  `Throwable` is an ordinary object with ONE reference field (`message`), a real `super` chain and
  Kotlin's own `toString` — the qualified name, and `: message` after it when there is one. Six
  classes are provided by the runtime with Kotlin's own chain: `Throwable`; `Error` and `Exception`
  under it; `RuntimeException` under `Exception`; `IllegalStateException` and
  `IllegalArgumentException` under `RuntimeException`; plus `NotImplementedError` under `Error`.
  The chain is what a `catch` clause will match against, by the `kt_is_instance` that already walks
  `super` — so the design of `catch` is fixed by this, and nothing about it is settled by the
  reporting behaviour above.
  These classes are declared in no file krusty compiles, so constructing one takes the path `Any()`
  already took: no layout and no constructor to call, the runtime allocates it. The no-argument and
  `message: String?` constructors are realized; a `cause` DECLINES, because this `Throwable` has no
  cause field and answering `null` to a program that passed one is worse than refusing it.
  `throw` is `Nothing`, so nothing reads a value from it.
  Tests: `tests/native_exceptions_e2e.rs` (all).

- **The stdlib functions that throw raise Kotlin's own exception, through the same path a written
  `throw` takes.** `TODO()` raises `NotImplementedError`, `error(message)` and a failed `check`
  raise `IllegalStateException`, and a failed `require` raises `IllegalArgumentException` — each
  with the message the stdlib specifies, and each handed to the same runtime entry point as
  `throw e`. The realization is not a convenience: `error(m)` IS `throw IllegalStateException(m)` in
  Kotlin, so a `catch` must not be able to tell them apart, and one object with one report is what
  keeps that true. (These reported a `krusty:` line of the runtime's own while there was no
  `Throwable` to report.)
  `TODO()` is a `Nothing`, so the caller's bottom-value contract takes over from the call.
  The forms taking a `lazyMessage` still DECLINE. That parameter is a lambda of an `inline`
  declaration whose body is not here to splice, and Kotlin lets such a lambda return from the
  enclosing function — so invoking it as an ordinary function value would be a miscompile rather
  than a slower answer.
  Tests: `tests/native_throws_e2e.rs` (all), `tests/native_codegen_e2e.rs`
  (`a_throw_a_program_wrote_stops_it_and_says_what_happened`).

- **`joinToString()` is realized only with every parameter at its default.** The stdlib declares
  six parameters, all defaulted, and the native backend has no `$default` synthetic of a dependency
  to call — so the defaults would have to be written into the backend, and the only set worth
  writing is the whole-declaration one a program gets by passing nothing: `", "` between the
  elements, nothing around them, no limit, and each element rendered by its own `toString`. A call
  that passes anything declines, with the argument it passed still in sight.
  A range joins the same way a list does: the member is declared on `Iterable`, and the runtime's
  walk dispatches on the descriptor rather than on the static type.
  Tests: `tests/native_lists_e2e.rs` (`a_list_joins_to_a_string_with_the_default_separator`,
  `joining_renders_each_element_through_its_own_to_string`,
  `a_range_joins_the_same_way_a_list_does`, `joining_with_an_argument_still_declines`).

- **The rest of `Iterable`'s members are walks the runtime makes, and TEXT does not reach them.**
  `any`, `all`, `none`, `count`, `filter`, `filterNot`, `first`/`firstOrNull`/`last` with a
  predicate, `fold`, `sumOf`, `forEachIndexed`, `toList`, `reversed`, `contains`, `indexOf` and
  `plus` each take the function value the program wrote and hand it to the runtime, whose walk calls
  its `invoke` per element. Which iterable the receiver is stays the runtime's question, as it is
  for iteration itself: a list, a range, an array and a lazy `withIndex()` all reach one entry point
  and the descriptor decides.
  A predicate is asked exactly as often as Kotlin asks it — `any`/`all` stop at the element that
  settles the question, `filter` asks once per element — because a predicate may have a side effect
  and the count is observable.
  Nothing asks the iterable for a SIZE: a lazy walk has none to give, and a filter's answer is
  shorter than its source by definition. Each collects into the growable list the runtime already
  has and freezes it into a read-only list, which IS its array, so no spare slot is ever visible.
  `sumOf` is chosen by the CALL's own type, not by the declaration: a jar provider realizes a
  function type as `Function1` and the width the selector answers is no longer written there.
  Summing at one width and narrowing afterwards is a different answer on overflow, so a width with
  no entry point declines rather than borrowing another's.
  `plus` answers a NEW list and never changes the receiver; whether it appends one element or
  concatenates is the PHYSICAL parameter's answer, exactly as it is for `plusAssign`.
  TEXT is excluded by name. A string wears the iterable role so `for (c in s)` walks it, but
  `s.contains(t)`, `s.reversed()` and `s.first()` are questions about TEXT — one text inside
  another, a text reversed, its first unit — and the string entry points answer them. A collection's
  answers to those names are about its ELEMENTS, so handing a string to them would compare a `Char`
  against a whole string.
  `IntRange.reversed()` is the RANGES facade's and answers a progression, not a list; it is a
  different member under the same name and is claimed by the range path before this one.
  Tests: `tests/native_collection_walks_e2e.rs`.

- **An array answers the collections facade's `content…` questions and `reversedArray`.** An
  array's own `equals` is identity, which is why `contentEquals`, `contentHashCode` and
  `contentToString` exist at all; each is the runtime's, reading the element type from the array's
  descriptor. `reversedArray` answers an ARRAY wearing the receiver's own descriptor — elements
  copied by the descriptor's stride, so a primitive array stays primitive — where `reversed()`
  answers a list of boxes. Which receiver a facade member is about is the CALLER's question: the
  facade declares the same names over lists, sequences and ranges, so the owner cannot say.
  Tests: `tests/native_collection_walks_e2e.rs`
  (`an_array_reverses_into_an_array_and_answers_the_content_questions`).

- **A floating-point range is a pair of bounds, not a progression.** `0.0..2.0` answers a
  `ClosedFloatingPointRange<Double>`, and there is no next floating-point number for Kotlin to name
  — so there is no walk and no step, only the question `value in it`. It therefore gets a shape of
  its own in the runtime rather than joining the integral ranges, whose bounds are 64-bit integers
  read at the element's signedness.
  A `Float` range is stored at `Double`: widening a float is exact and order-preserving, so every
  comparison answers what float comparison would. A bound is answered at `Double` and narrowed back
  through the ELEMENT before it reaches the site, because `ClosedRange`'s own declaration types
  `start` as the erased `T` and the value is boxed there — a `Float` left in a `Double` box reads
  back as another number.
  NaN needs no case of its own. `contains` is `value >= start && value <= end` and `isEmpty` is
  `!(start <= end)`, which is Kotlin's own `lessThanOrEquals` on these types, so a NaN bound makes
  the range empty and a NaN value belongs to nothing. Equality follows: every empty range equals
  every other, which is what makes `NaN..NaN` equal itself though NaN equals no number.
  Both `ClosedFloatingPointRange` and `ClosedRange` are INTERFACES a user class may implement, which
  is why the integral table is keyed on the owner and this one on the RECEIVER; a file that declares
  such an implementation is declined at every member asked of a type it implements itself, before
  this is reached.
  Tests: `tests/native_floating_ranges_e2e.rs`.

- **A map is two lists side by side, and a set is a map with no values.** `mapOf` answers a
  `LinkedHashMap`, whose iteration, `toString` and `keys` are in INSERTION ORDER — so the keys live
  in a growable list with the values beside them at matching positions, rather than in buckets.
  A set is the same object without the values, which is what `LinkedHashSet` is.
  Lookup is LINEAR, by `equals`. Kotlin's is by hash, and the difference is speed and nothing else:
  a hash map answers the same question, and the maps a box test writes hold a handful of entries.
  What a hash table would not give is the order, and order is the observable part.
  The unordered spellings — `hashMapOf`, `HashSet()`, and the `java.util` names a jar provider hands
  over for them — answer this object too, because their order is unspecified and insertion order is
  one of the orders left unspecified. Nothing may pin what one of those prints.
  `put` on an existing key keeps its POSITION, which is what a `LinkedHashMap` promises. `keys`,
  `values` and `entries` are SNAPSHOTS where Kotlin's are views — the same trade `toList()` on an
  array makes, visible only to a program that keeps one across a write. A `Map` is walked as its
  ENTRIES, which is what Kotlin's `Map.iterator()` extension answers and what `for ((k, v) in m)`
  destructures.
  Equality says nothing about order, for a map or a set, and the hash is a sum over the entries for
  the same reason. A null VALUE is a value and not an absent key, which `containsKey` is for.
  Which call reaches the runtime is decided by the RECEIVER, never by the owner: `get` is declared
  on `Map`, which a user class may implement, and a file that declares one is declined at every
  member asked of a type it implements itself.
  Tests: `tests/native_maps_e2e.rs`.

- **`compareTo` asked of a `Comparable` receiver is the DESCRIPTOR's answer, and a file with its own
  `Comparable` declines.** The receiver's descriptor says what to compare, exactly as `equals` and
  `toString` on such a receiver already read it: a boxed primitive at its own width, a string by
  UTF-16 unit, the unsigned integers read unsigned. Each side is unboxed at its own descriptor's
  field rather than through one reader, because the boxes share a union and a `Byte`'s payload read
  as an `Int` reads bytes nothing wrote.
  Kotlin's order on the floating types is TOTAL where the machine's `<` is not — `-0.0` below `0.0`,
  every NaN above everything including itself — and that is the order `Comparable<Double>` answers
  with. The same two values compared as PRIMITIVES are equal, and both answers are right: which one
  a program gets is what its static type decides.
  Two values of different types have no order between them, which `Comparable<Any>` runs into; the
  JVM raises `ClassCastException` there and so does this.
  A file that declares a `Comparable` of its own DECLINES: an object of the program's could stand
  behind that type and no static type tells it from one the runtime made. Three shapes count, none
  of them an override edge — a class that NAMES `Comparable` among its supertypes without overriding
  anything there (`interface A : Comparable<A>`, whose implementor overrides `A`'s spelling), an
  ENUM, whose comparison `kotlin.Enum` supplies by an ordinal this generator lays out and the
  runtime cannot read, and a class reaching `Comparable` through a supertype declared elsewhere,
  whose evidence is an override of an external `compareTo`. Naming it anywhere in the file is enough
  without walking the hierarchy: the class that names it is itself declared there.
  Tests: `tests/native_comparable_e2e.rs`.

- **`kotlin.math.abs`, the floating-point bit conversions, and a walkable `StringBuilder`.** Each is
  exact, and each has one detail a naive realization gets wrong.
  `abs` on an integral type WRAPS at the minimum — `abs(Int.MIN_VALUE)` is `Int.MIN_VALUE`, because
  there is no positive value to answer with. On a floating one it clears the SIGN BIT rather than
  negating: `-0.0 < 0.0` is false, so `if (x < 0) -x else x` hands back the negative zero it was
  given, and `abs(NaN)` must stay a NaN.
  `toBits` differs from `toRawBits` in one respect: every NaN answers the canonical one, the same
  collapse `equals` and `hashCode` make. `fromBits` is answered in the KLIB lane only — it is an
  extension of the companion object, and while a klib call reaches it with that object unread, a jar
  call materializes the object first, which this target cannot do for a type declared in no file.
  A BUILDER is walkable text. `for (c in StringBuilder("OK"))` is Kotlin's own, and the walk is the
  string's: the chars iterator reads its element through `kt_string_get` and its bound through
  `kt_string_length`, both of which answer for either shape — and it re-reads that bound every step,
  which is what lets a loop that shortens the builder stop where Kotlin's stops. `setLength` counts
  UTF-16 units: shorter truncates on a character boundary, longer pads with NUL.
  `assertSame` is IDENTITY where `assertEquals` is equality — two strings with the same text are
  equal and are not the same object. Its failure wording was read off the reference toolchain rather
  than guessed.
  Tests: `tests/native_math_bits_e2e.rs`.

- **`asSequence()` answers a lazy wrapper, and a sequence is offered fewer members than an
  iterable.** The wrapper holds the source until something asks it for an iterator, which is the one
  member `Sequence` declares — and it holds a SOURCE rather than a cursor, so the same sequence walks
  again from the start.
  It is its own type rather than the source itself, because what separates a `Sequence` from an
  `Iterable` here is which members may be asked. Every walk this runtime has is EAGER, and an eager
  `map` on a sequence is not Kotlin's: the transform would run for every element where Kotlin runs it
  per element consumed, which a side effect sees and an endless sequence never survives. So a
  sequence takes only the members whose answer is the same either way — its iterator and the lazy
  `withIndex` — and the rest DECLINE at the call site, where the type the program named is in sight.
  That is a decline and not a slower answer: answering eagerly would be a different program.
  `equals` and `hashCode` are IDENTITY, which is what Kotlin answers, `Sequence` declaring neither.
  A class of the PROGRAM that implements `Sequence` is walked through the very thunks a program's
  `Iterable` is walked through — its one member is `iterator()`, the descriptor carries the slot and
  the dispatch is virtual, so who made the object never matters — and it carries the same narrowing:
  the shape a class answers for is read off its override edges, so a receiver typed by the program's
  own sequence class is offered the lazy members and declines the eager ones exactly as a receiver
  typed `Sequence` does. What makes an eager `map` wrong is the type the program named, not who made
  the object behind it.
  Tests: `tests/native_sequences_e2e.rs`, `tests/native_sequence_walks_e2e.rs`.

- **A built-in type's companion object is one static object per companion.** `Int.Companion` and its
  relatives are declared in no file krusty compiles and carry no state — every member of one is a
  constant the frontend folds — so the only thing a program can observe about one is its IDENTITY,
  which the corpus asks directly (`o === Int.Companion`, and `Int` written as a value being the same
  object). Static storage gives that: the collector never sees it as an allocation and the address is
  stable for the program's life, exactly as `Unit` is. Each gets a DESCRIPTOR of its own, never a
  shared one, because `Int.Companion === Long.Companion` must be false and a shared type would also
  make `is` answer for the wrong one.
  Tests: `tests/native_builtin_companions_e2e.rs`.

- **A narrow integral constant keeps its own WIDTH in common IR.** `FirConstant` has no case
  narrower than `Int` and no unsigned one, so the constant expression's checked TYPE is the only
  thing carrying the width — which is why `lower_constant` already reads it to tell `UByte` from
  `UInt`. It did not read it for the signed narrow pair, so a `Byte`-typed constant was recorded as
  an `Int`.
  That is invisible to a backend which boxes by the expected type and wrong for one which boxes by
  the constant's SHAPE: the native backend boxed `Byte.MIN_VALUE` as an `Int`, so
  `Byte.MIN_VALUE as Any is Byte` answered false and two equal bytes compared unequal once boxed
  through a generic parameter. The JVM lane passed the same programs throughout, which is why this
  went unseen until the companion objects let those cases compile at all.
  Arithmetic is unaffected: `Byte + Byte` is an `Int` in Kotlin, and that promotion is the
  operation's, not the constant's.
  Tests: `tests/native_builtin_companions_e2e.rs`
  (`a_narrow_constant_boxes_as_its_own_type`, `a_narrow_constant_still_promotes_for_arithmetic`).

- **`x!!` has the operand's type with the nullability taken off, and the type table says so.** The
  lowering already knew it — it unboxes a nullable primitive there — but the TYPE table had no arm
  for the assertion, so a CONSUMER of `x!!` was left holding a value it could not name. `c!!.toInt()`
  on a `Char?` then reached the conversion with a reference where a machine value was required, and
  declined as "a value of undetermined type (carried as `i64`) where a `i32` is required". A
  `lateinit` read has the same shape: it yields its operand, and only the guard differs.
  Tests: `tests/native_not_null_assert_type_e2e.rs`.

- **A zero-argument member of a type this file implements ITSELF is dispatched by what the receiver
  turns out to be.** The runtime answers such a member for the objects IT makes, and a class of the
  program's is not one of them — no static type tells the two apart, which is why the answer is the
  runtime's at all and why this was a decline.
  Where the member takes NO ARGUMENTS the choice can be made at the call site: the file knows every
  class of its own that could stand behind that type, so the receiver is tested against each,
  dispatched on that implementor's own slot when it matches, and handed to the runtime entry point
  otherwise. No program-wide slot number is needed, which is what the implementation plan first
  assumed — the decline is raised precisely when the implementor is in THIS file, so its slot is one
  this file already assigned. A subclass needs no entry of its own either: `is` walks the super
  chain, and a subclass's vtable has already replaced the slot the dispatch reads.
  ZERO arguments on purpose. An argument would have to cross at the DECLARATION's carriers in one
  arm and at the runtime entry point's in the other, and reconciling those is more than a receiver
  test; `iterator`, `hasNext` and `next` take none, which is this whole group. A member with
  arguments keeps declining.
  The receiver is evaluated ONCE, before the tests, and both arms read that value — a receiver with
  a side effect must not be evaluated per branch.
  The runtime arm is emitted and is not yet REACHABLE: a file declaring a collection of its own
  still declines every collection member asked of a concrete runtime type, through a blanket
  file-level guard that predates this, so no runtime iterator can be obtained inside such a file.
  The arm is still what to emit — it becomes live when that guard is made receiver-precise.
  Tests: `tests/native_implemented_dependency_dispatch_e2e.rs`.

- **`assertEquals` compares booleans structurally, like everything else it compares.** It is
  generic, so `assertEquals(true, true)` has `Boolean` as its first parameter after substitution.
  Reading the parameter to decide how the operands cross handed two raw machine values to an entry
  point that reads them as references, and dereferenced 1 as a pointer. Only `assertTrue` and
  `assertFalse` take a `Boolean` as one, and the SYMBOL says which, where the parameter cannot.
  Tests: `tests/native_throws_e2e.rs` (`the_equality_assertion_compares_booleans_as_values`).

- **Slicing and ordering a string on the native target are by UTF-16 unit.** A krusty string holds
  UTF-8 and Kotlin indexes by UTF-16 code unit, so `substring`, `subSequence` and `compareTo` all
  walk the text rather than its bytes: `é` is two bytes and one unit, `𝄞` four bytes and two.
  A slice SHARES the receiver's storage — a substring is a view, and the text it names is already
  there. Slicing between the halves of one character is a loud failure: Kotlin answers that with an
  unpaired surrogate and UTF-8 has no encoding for one, so there is no string to hand back and
  saying so beats handing back a different text.
  `compareTo` answers Java's magnitude and not just a sign — the difference of the first units that
  differ, or of the lengths when one string is a prefix — because a program may print it.
  `removeSuffix` is settled by BYTES, which is sound because UTF-8 is a prefix code: two texts end
  the same way exactly when their trailing bytes do.
  `CharSequence.length` is a string's length here. Every `CharSequence` this target can produce is
  a string, which is the position the member table already took for `CharSequence.get`.
  Tests: `tests/native_strings_e2e.rs`
  (`a_substring_of_text_outside_ascii_counts_the_units_kotlin_counts`,
  `a_subsequence_is_the_same_slice_and_answers_its_length`, `strings_order_by_their_units`,
  `a_suffix_is_removed_only_when_the_string_ends_there`,
  `the_comparison_magnitude_agrees_with_the_jvm_backend`), `tests/native_codegen_e2e.rs`
  (`a_slice_between_the_halves_of_one_character_fails_loudly`).

- **`kotlin.text`'s questions about a string are the runtime's, and the case-INSENSITIVE forms
  decline.** `isEmpty`, `isNotEmpty`, `isBlank`, `isNotBlank`, `trim`, `trimStart`, `trimEnd`,
  `startsWith`, `endsWith`, `contains`, `repeat`, `reversed`, `first` and `last` each need a walk of
  the encoding, which is where `length` and `s[i]` already live.
  Which walk each one needs is the whole of the decision. Emptiness is about BYTES — a text has
  zero UTF-16 units exactly when it has zero bytes. `startsWith`, `endsWith` and `contains` are
  about bytes too, for the reason `removeSuffix` is: UTF-8 is a prefix code, so a match can neither
  begin inside a character nor straddle one. Blankness, trimming and reversal are about
  CHARACTERS — reversing by UTF-16 unit would split a surrogate pair, and Kotlin's own answer keeps
  it whole. `first` and `last` are about units, which is what a `Char` is.
  Whitespace is Kotlin's `Char.isWhitespace()`, the UNION of Java's `isWhitespace` and
  `isSpaceChar`, so the non-breaking spaces `isWhitespace` alone excludes are whitespace here.
  A trimmed STRING shares the receiver's storage as a substring does; a trimmed BUILDER is copied,
  because a later `append` may replace the array its text lives in.
  `ignoreCase = true` DECLINES: it asks about Unicode case folding rather than about text, and the
  runtime holds no case table. The flag has a default and the two providers hand it over
  differently — a klib call materializes a constant `false`, a jar call leaves the argument out —
  so the call site reads its ARGUMENTS rather than the signature, and both spell the same answer.
  A raise inside one of these is followed by a RETURN: `kt_throw` records the exception for the
  call site and comes back, so `first()` on empty text must not go on to ask for index 0 — that
  raise would take the place of the one the program should see.
  Tests: `tests/native_string_members_e2e.rs`.

- **A primitive's member, asked of a value that arrived as an object.** Two shapes on the native
  target, and what separates them is who knows which primitive is in the box.
  `n.toInt()` where `n` is a `Number`: the site could type it only as `Number`, so the DESCRIPTOR
  is the only thing that knows, and the runtime reads it. Kotlin's own conversion rules hold —
  a floating-point source saturates to the target's nearest end, `NaN` answers zero, and a
  narrower integer target goes through `Int` first — none of which is a C cast.
  `x++` where `x` is an `Int?`: the frontend selected `Int.inc()`, so the owner already says what
  is in the box and the receiver is taken at that type directly. It must NOT make the round trip
  through a reference: boxing reads the source's own type and unboxing reads the target's, so a
  disagreement between them comes back as changed bits rather than as a decline.
  A step wraps in the width of the type it steps, `Byte.MAX_VALUE.inc()` being `Byte.MIN_VALUE`.
  Tests: `tests/native_boxed_numbers_e2e.rs`
  (`a_number_holding_a_double_saturates_and_answers_zero_for_nan`,
  `a_number_narrows_through_int_the_way_kotlin_defines_it`,
  `a_boxed_int_steps_through_the_member_it_selected`,
  `a_step_wraps_in_the_width_of_the_type_it_steps`,
  `the_conversion_table_agrees_with_the_jvm_backend`).

- **The small-value box cache is keyed by the whole value.** Kotlin lets a program observe box
  identity in -128..127, so the native runtime keeps one static object per value in that range. The
  slot has to be chosen from the value itself and not from its low word: `Long.MIN_VALUE`'s low
  32 bits are zero and `Long.MAX_VALUE`'s are -1, so a truncating key put them in the slots for 0
  and -1 and handed those boxes back — `"${Long.MIN_VALUE}"` printed `0` once anything had boxed a
  zero, and `boxed(0L) == boxed(Long.MIN_VALUE)` answered true.
  Test: `tests/native_codegen_e2e.rs`
  (`a_long_outside_the_cache_is_not_confused_with_one_inside_it`).

- **`by ::foo` delegates to the reference's own `get` and `set`.** The stdlib declares four
  operators in `kotlin/PropertyReferenceDelegatesKt` — `getValue` and `setValue` on `KProperty0`
  and on `KProperty1` — each an `inline` one-liner over the reference's own member. A dependency
  `inline` body is not there to splice, so the native generator realizes them instead, and which of
  the two overloads a site means is read off the RECEIVER's type: a `KProperty1` is handed the
  delegating property's owner as its receiver, a `KProperty0` carries its own or needs none. The
  `property` metadata operand these operators ignore is still evaluated.
  The delegate is the reference and not a copy of the value, so each read asks the property again —
  which a source-written getter makes visible.
  Tests: `tests/native_property_reference_e2e.rs`
  (`a_property_delegating_to_a_bound_reference_reads_through_it`,
  `a_property_delegating_to_a_top_level_reference_reads_and_writes`,
  `a_property_delegating_to_an_unbound_reference_is_handed_the_owner`,
  `delegating_to_a_reference_reaches_the_source_accessor`).

- **A class's initializers are bodies too.** The native generator declares a type for every
  property reference and every local delegated property's metadata in a pass over the file, and
  those passes have to finish before the FIRST body is defined — not before the first top-level
  function is. A class's property initializers and `init` blocks are lowered as part of its
  constructor, so `class A { val r = C::z }` reaches a reference site while the classes are being
  defined; declaring afterwards left exactly those sites unrealized and the program declined.
  Tests: `tests/native_property_reference_e2e.rs`
  (`a_reference_written_in_a_class_body_is_realized`, `a_reference_in_an_init_block_is_realized`,
  `a_local_delegated_property_of_a_class_body_is_realized`).

- **`KCallable.name` needs no reflection metadata on the native target.** A callable reference's
  object carries the declaration's name in a member of its own, and at the SAME slot a property
  reference's `name` takes — which is what lets a read through `KCallable`, the one type both wear,
  reach it without the site having to know which of the two it has. `KFunction{N}` is admitted
  there beside `KProperty{N}`; a plain `Function{N}` is not, because an ordinary lambda wears that
  type and has no such member. Where the REFERENCE is the read's receiver the name is folded at
  compile time instead, since the declaration is in the very node being lowered, and the two
  answers are required to agree. A constructor reference answers `<init>`, which is the name Kotlin
  gives it. The receiver is still EVALUATED — `x::foo.name` runs `x` and then answers the constant
  — because the constant is the answer, not the expression.
  Tests: `tests/native_callable_name_e2e.rs`
  (`a_top_level_function_reference_answers_its_name`, `a_constructor_reference_is_called_init`,
  `the_bound_receiver_is_still_evaluated`,
  `a_reference_reaching_the_read_through_a_variable_answers_the_same_name`).

- **`x in a..b` builds no range.** The checker leaves the membership test as its BOUNDS rather than
  as a range object, so the whole of it is two comparisons — and each form puts them somewhere
  different: `..<` excludes its high end, and `downTo` writes its ends the other way round, so the
  low one is the second. `Char` and the unsigned integers compare unsigned, `Char` because its own
  carrier is narrow enough that a signed comparison would call its upper half negative.
  Both ends are still EVALUATED, and before the subject: `x in a..b` is `(a..b).contains(x)`, and a
  receiver is evaluated before an argument. Comparing without building a range must not change
  that, which is why all three operands are evaluated before any comparison rather than as each one
  is needed.
  Tests: `tests/native_ranges_e2e.rs` (`a_range_membership_test_builds_no_range`,
  `a_range_membership_test_reads_its_counter_the_right_way`,
  `a_range_membership_test_evaluates_its_bounds_before_its_subject`).

- **A spread makes a vararg array whose length only run time knows.** A `vararg` call normally
  builds its array from elements the generator can count, so each has a constant offset. `f(a, *xs,
  b)` is as long as `xs` is: the length is summed at run time from the non-spread count plus each
  spread array's own, and the elements are placed at a running index — a spread by copying its
  elements in, the rest one at a time. The running index counts ELEMENTS, so the stride is what
  turns it into an address, and a spread copies by that same stride rather than by a pointer's
  width.
  The COPY is also the semantics: the callee's `vararg` array is its own, and a spread that passed
  the caller's array itself would let a write reach back through it.
  Tests: `tests/native_spread_e2e.rs`.

- **A `fun interface` method may be an extension, and that changes nothing about the object.**
  Kotlin lets the single abstract method take a receiver (`fun String.foo(): String`), and inside
  the lambda implementing it `this` is that receiver. The receiver is a declared PARAMETER of the
  interface method, so it arrives where every other argument does and the SAM thunk forwards it
  with the rest. The generator used to decline the shape on the strength of a flag saying a
  receiver was present, without asking whether that made any difference to it — it did not. Context
  parameters still decline, because those are operands the object genuinely does not carry.
  Tests: `tests/native_sam_e2e.rs`.

- **What a parameter is CARRIED as comes from the record, not from reading the body.** A `var` a
  closure captures is replaced by a cell, and the parameter still says `Int` because `Int` is what
  the programmer wrote; believing the declaration truncates a pointer into a 32-bit parameter.
  Common lowering RECORDS which parameters carry a holder, in `shared_capture_parameters`, and that
  record is the answer. Inferring it from the body instead — a body that reaches a holder through
  `RefGet`/`RefSet` is holding one — cannot see a parameter the body only PASSES ON: a lambda that
  does nothing with the cell but hand it to an object it constructs dereferences it nowhere.
  The failure is worth writing down because of how it presented. The truncation is invisible where
  it happens: the REFERENCE path still read the cell and rendered the right number, while every
  SCALAR use of the same variable read a different one. `"" + x` said `1` and `x == 1` said false,
  in the same expression. An array index is what shows the scalar path on its own.
  Tests: `tests/native_gc_stress_e2e.rs`
  (`a_capture_a_lambda_only_passes_on_is_still_a_cell`).

- **`map` and `forEach` are answered for whichever iterable the receiver holds.** Both are declared
  on `Iterable`, which a range wears as much as a list does, so they go through the same dispatch on
  the DESCRIPTOR that iteration itself does. `map` answers a list — an array with a header — so the
  runtime asks the iterable its size once and allocates once; the result list is built before the
  walk, because every element comes from a call that may collect, and the half-filled result has to
  be a root across each one.
  Tests: `tests/native_lists_e2e.rs` (`map_and_for_each_walk_whichever_iterable_they_are_handed`,
  `a_mapped_list_holds_what_the_transform_made`).

- **`by lazy { … }` is where the runtime calls back into emitted code.** A `Lazy` holds its
  initializer until the first read and its value afterwards, with one bit saying which; the
  initializer is dropped once it has run, as Kotlin's own `SynchronizedLazyImpl` does, since keeping
  it would keep its captures alive for nothing. Computing the value means CALLING the initializer,
  which is a function value — so the runtime dispatches through the one vtable slot a function value
  declares beyond `kotlin.Any`'s three, where krusty's generator puts `invoke`. That slot is the
  contract between the two, and nothing but a function value ever reaches something that calls
  through it.
  Only the one-argument `lazy` is realized. The overloads taking a thread-safety mode or a lock
  decline by arity: this target has no threads yet, and answering one of those as if it were the
  plain form would silently drop what the program asked for. `Lazy.toString` does not force the
  value — that is the whole point of its wording.
  Tests: `tests/native_lists_e2e.rs` (`a_lazy_value_is_computed_once_and_only_when_asked`,
  `a_lazy_initializer_reads_what_it_captured`,
  `a_lazy_holds_its_value_and_answers_before_it_has_one`).

- **`a to b` is a runtime object, and a checked property read of one is decided by its receiver.**
  A `Pair` is two references with the three `kotlin.Any` members answering componentwise, as
  Kotlin's data class does; `component1`/`component2` are the same two questions under the names a
  destructuring uses. `Triple` is not one of these. What the pair added to the rule that the
  RECEIVER decides is a case where a name alone genuinely collides: a range declares `first` too,
  and a getter guard that read only the name answered a range's bound out of a pair's header.
  Tests: `tests/native_lists_e2e.rs` (`a_pair_carries_two_values_and_answers_by_them`,
  `a_pair_destructures_through_its_components`), and `tests/native_ranges_e2e.rs`, which is what
  caught the collision.

- **A `Nothing`-typed producer that returns is a checked bottom value, and the completion mode is
  what says so.** `Nothing` promises there is no value, and most producers keep the promise by never
  coming back; two do not. A call whose generic result is SUBSTITUTED to `Nothing` really produces
  something — the substitution is erased at the call — and kotlinc carries on with it. A call that
  genuinely answers `Nothing` has no continuation, so a path reaching past one is a callee that
  lied. Common lowering tells the two apart once, from the producer rather than its spelling, and
  krusty's native target realizes that decision in BOTH positions rather than re-deciding by
  position: a substituted result is handed to whatever asked and discarded in statement position,
  and a genuine one ends the path with a runtime failure. The JVM throws
  `KotlinNothingValueException` there; this target does NOT raise it as a `Throwable`, unlike the
  stdlib's own throwers, because reaching it means a callee lied about its type — a defect in what
  was emitted rather than something a program is entitled to catch.
  Tests: `tests/native_bottom_value_e2e.rs`.

- **An unbroken `while (true)` is not left.** Nothing branches to the exit of a loop whose condition
  is never false and which no `break` leaves, so the exit is taken out of the graph rather than left
  unreachable in it, and the statement after the loop — including the end of a function written this
  way — is not a position. This is how a `Nothing`-returning function is written, and Kotlin reads
  it the same way, which is why it types such a body `Nothing` and lets the function declare it.
  Tests: `tests/native_bottom_value_e2e.rs`
  (`a_loop_that_is_never_false_is_left_only_by_a_break`).

- **A `tailrec` the rewrite did not finish is declined, not emitted.** The modifier is a promise
  about the STACK: the source chose a recursion depth because the loop rewrite will remove the
  recursion, so a backend that emits an ordinary call does not answer slowly, it crashes. Whether
  the rewrite was ATTEMPTED is a property of the declaration (a receiver to re-bind, context
  parameters); whether it FINISHED is a property of the lowered body, because a self-call in a
  position the rewrite does not descend into — inside a loop, under a `try` — survives in a
  function loop-rewritten everywhere else. Common lowering now records both, and krusty's native
  target declines what is left recursive: how deep a native stack goes is the machine's business,
  and a gate must not depend on it.
  Tests: `tests/native_tailrec_e2e.rs`.

- **A reference names a property, not a slot.** A top-level property with source-written accessors,
  or a delegated one, has no slot for a reference to read: its value is computed or lives in the
  delegate. The reference reaches it the only way anything does — through the accessor pair the
  checked lowering built — which is the same path an extension property's reference takes, and the
  only difference between them is whether that accessor leads with a receiver. A site with none has
  none to pass, bound or unbound. This is also what a TOP-LEVEL delegated property needs before any
  `::` is written: the metadata its `getValue(thisRef, property)` is handed is that same reference
  object.
  Tests: `tests/native_property_reference_e2e.rs`
  (`a_top_level_property_with_accessors_is_referenced_through_them`,
  `a_top_level_delegated_property_asks_its_delegate_through_a_reference`).
- **A signature-pass block statement never fails the block's result on its own.** The solver
  evaluates a block's statements for the constraints they contribute (an anonymous object's
  member selection, a scoped generic binding) and then its result expression. A statement that
  failed used to fail the whole signature: `fun t() = runTest { every { … } returns emptyList() }`
  lost its (fixed, `TestResult`) return type to a member selection inside the body, and every use
  of the test then cascaded. kotlinc infers `fun f() = run { broken(); 1 }` as `Int` and reports
  `broken()` from the body check; the solver now does the same — a failed effect is dropped, a
  later expression that reads it fails on its own, and the effect's Pass-1 diagnostic is not
  reported because the declaration did not fail (Pass 2 reports the body). Test:
  `streaming_signature_tests::a_failing_statement_does_not_fail_the_inferred_result_of_its_block`.
- **A signature-pass member call hands its parameter to a nested generic call.** A nested call
  argument (`emptyList()`, `mapOf()`) is probed with its formals defaulted (`List<Any>`) and is
  marked `contextual_call`; the top-level path re-selects it under the selected parameter, but
  the member path declared every typed argument final and selected `returns(value: List<String>)`
  against `List<Any>` — `none of the following candidates is applicable` on every mockk
  `every { … } returns emptyList()` inside a `= runTest { … }` body. The member expectation phase
  now treats a contextual call like a postponed lambda: when every applicable overload agrees on
  the parameter at that slot (and it mentions no open formal), that parameter is the argument's
  expectation and the call is re-evaluated under it before selection. Test:
  `streaming_signature_tests::a_member_call_hands_its_parameter_to_a_nested_generic_call_in_pass_one`.
- **Equally specific candidates: a non-parameterized callable wins.** kotlinc's last tie-break
  (spec 11.7) applied to the receiver-less SAM selection: `assertDoesNotThrow(Executable)` beside
  `<T> assertDoesNotThrow(ThrowingSupplier<T>)` (JUnit, imported as a static) both take a `{ … }`
  through a SAM conversion at the same rank; krusty reported `overload resolution ambiguity` (eleven
  times in one real test file) where kotlinc selects the non-generic overload. The tie among
  maximal candidates now resolves to the single candidate without type parameters when there is
  exactly one; any other tie stays ambiguous. The selected call site is byte-identical to kotlinc
  (`invokedynamic` + `invokestatic` of the `Executable` overload); the synthesized lambda body
  still returns `Unit` (`getstatic Unit.INSTANCE; areturn`) where kotlinc's is `void` for a void
  SAM method — an open backend residue beside it. Test: `tests/non_generic_overload_wins_e2e.rs`.
- **A signature-pass block applies a call's `returns() implies (x != null)` contract.** An
  expression-bodied function whose result is a block-ish lambda (`fun t() = runBlocking { … }`,
  `= runTest { … }`, `= run { … }` — the shape of most test functions) gets its return type from
  the Pass-1 signature solver, which evaluates the block on its own compact model. That model
  narrowed `if (x != null)` but not a contract call: after `assertNotNull(status)` a later
  `status.status` was selected on the declared `ServerRuntime?`, the member selection failed with
  a real diagnostic, and the whole function's signature failed — `unresolved reference` on a
  member kotlinc accepts, once per such read (fourteen in one real test source set), plus a
  `cannot infer the return type` on the test itself when the read was the result. The extractor
  now rebinds every lexical value (a local or a parameter) that is a positional argument of an
  expression-statement call through a `ContractNarrowed` node; at evaluation the node consults the
  contract of the callable actually selected for that call (`selected_call_contracts`, recorded
  by origin at selection) and yields the definitely-non-null type only when an effect
  `returns() implies (param_i != null)` names that argument. `assertNotNull`, `requireNotNull`,
  `checkNotNull` and a same-module source contract all qualify; a same-module `assertNotNull`
  without a contract narrows nothing and the read is rejected as kotlinc rejects it. Source
  arguments are mapped to the selected declaration parameters, including named arguments and
  repeated positional vararg elements, before a contract parameter index is applied. The body
  check still applies its own flow narrowing in Pass 2; Pass 1 only has to agree on the result type, so the emitted
  bytes of such a body do not change under this rule (the inline `run { … }` shape itself still
  carries the known inline-lambda residues: the `$i$a$-run` marker local, the inlined
  `requireNotNull` body, and a `checkcast` kotlinc omits on an unused generic result). Tests:
  `streaming_signature_tests::expression_body_narrows_a_value_after_a_not_null_contract_call`,
  `…::a_shadowing_callee_without_a_contract_does_not_narrow_in_pass_one`,
  `tests/contract_smartcast_e2e.rs::expression_bodied_function_narrows_after_require_not_null`.
- **A selected call re-enters an argument only when the expectation can change it.** After a
  callable is selected, each argument is re-checked under the selected parameter type so the
  expectation can reach a nested generic call's result variables (`"OK" to emptySet()` under
  `Pair<Any, Set<Any>>`), and a nested generic call whose probe was only an erased bound is finished
  from its own arguments. Both re-entries happened unconditionally, so every level of
  `mapOf("k" to mapOf("k" to …))` redid its whole subtree twice — once for `to`, once for `mapOf` —
  exponential in the nesting: six levels took 70 s in a release build and a 230-line test file with
  such literals stalled the editor's analysis worker for the better part of an hour. Three rules now
  bound it, all idempotence arguments rather than new inference: (1) an argument whose recorded type
  already EQUALS the selected expectation is committed as that type without re-entry (the recorded
  type, not the probe's older encoding of the same shape, is what a re-entry used to replace);
  (2) an expectation of `Any`/`Any?` constrains no result variable (a literal declared
  `Map<String, Any?>` hands `Any?` to every nested value); (3) a nested generic call whose probe is
  concrete — no formal, no error slot, not the top type — was already typed from its own arguments
  and is not finished again. Nine levels: 21.7 s CPU → under a second, and the depth curve is flat.
  Tests: `tests/nested_generic_call_expectation_e2e.rs` (a depth-9 time bound, and the `emptySet()`
  case that must still be re-entered).
- **A classpath entry that cannot be opened is reported, in kotlinc's words.** The classpath reader
  drops a `-cp` entry it cannot open without a message, so a missing, truncated or unreadable jar
  surfaced only as `unresolved reference` on every import from it — a harness run under load
  produced exactly that for two serialization tests and nothing tied it to the jar. kotlinc 2.4.10
  warns and continues in both cases: `warning: classpath entry points to a non-existent location:
  <path>` for a missing entry, and `WARN: Error while reading zip file: <path>` (with a stack trace)
  for an archive that does not open; a truncated but structurally valid archive it reads silently.
  krusty prints the first message verbatim and `warning: cannot read classpath entry <path>: <why>`
  for an existing entry that cannot be read or, for a `.jar`/`.zip`, does not open as an archive;
  a directory only has to be listable and any other file (a `lib/modules` jimage) only has to open.
  Compilation continues, as in kotlinc, so a stray unusable jar never changes the output of a module
  that does not need it. Checked before analysis on the user's `-cp` entries only
  (`cli::classpath_entry_problems`). Tests: `tests/classpath_entry_warning_e2e.rs`.
- **A star projection is the `out` projection of its readable bound.** `Resp<*>` is `Resp<out
  Any?>`, and a Java wildcard `Resp<?>` reaches Kotlin as `Resp<out Any!>`. Inside an INVARIANT
  outer argument (`Publisher<MutableHttpResponse<*>>` against Micronaut's Java
  `Publisher<MutableHttpResponse<?>>` from `ServerFilterChain.proceed`) the two spellings are one
  type argument, whichever way the assignment runs; krusty's invariant-argument equality
  (`assignable::same_type_argument`) compared projection kinds literally and rejected both
  directions (`argument type mismatch: actual type is 'Publisher<MutableHttpResponse<*>>', but
  'Publisher<MutableHttpResponse<out Any!>>' was expected`, three times in one test file). A
  `StarProjection(bound)` now equals an `OutProjection(other)` when the bound and the other are the
  same flexible type. Not a subtyping change: `Foo<*>` with a narrower declared bound is still not
  `Foo<out Any?>`. Emitted bytes for the shapes are unchanged by this rule; the residues beside them
  (a bridge method's debug tables, constant-pool order) predate it. Test:
  `tests/star_projection_wildcard_e2e.rs` (a Java fixture with a wildcard result and parameter,
  declared, stubbed, and passed, run on the JVM).
- **A lexical local or parameter beats an implicit receiver's member of the same name, even a
  receiver introduced inside its scope.** `val headers = authHeaders(); client.get(url) {
  headers.forEach { (k, v) -> header(k, v) } }` reads the local map, not
  `HttpRequestBuilder.headers`, in kotlinc; krusty probed the receivers introduced after the
  binding (`implicit_receivers_before_value_binding`) for every binding origin, so a lambda with
  receiver silently rebound any outer local or parameter its receiver happened to name
  (`headers`, `url`, `body`, `size` in every ktor builder), and the reads on the wrong type cascaded
  (`unresolved reference 'forEach'`, `cannot destructure`, `unresolved reference 'keys'`). The probe
  now runs only for a receiver-derived binding — an outer class's own property, which an inner
  implicit receiver's member does shadow, as before. A lexical local remains lexical after a local
  class materializes its capture as `ClassStorage`. Bytes for the shapes are unchanged beside the
  known captured-`Ref` initialization residue. Test: `tests/local_shadows_receiver_member_e2e.rs`
  (a local, parameter, materialized local-class capture, and outer property, each pinned by the
  value the box run reads).
- **The signature evaluator's acyclicity guard clears on every exit, not only on success.** The
  solver guards against a cyclic compact graph by marking each expression while it is being
  evaluated. Inside the evaluation arms every `?` returned early WITHOUT unmarking, so a node that
  failed stayed marked. That was invisible while any failure aborted the whole declaration; once a
  failed statement effect is dropped and evaluation continues (the rule above), a later reader of
  the same node — `val (k, v) = it.split("=", limit = 2)` inside `= runBlocking { … }`, where the
  member call declines in Pass 1 and both destructured components read it — tripped the guard as a
  cycle and PANICKED the analysis worker (`exit status: 101`, five source sets of a real project).
  The arms now run inside one block whose result is judged after the guard is cleared, so a failed
  node is simply re-evaluated (and fails again) on a second read. Test:
  `streaming_signature_tests::a_failed_local_read_twice_in_a_block_does_not_trip_the_cycle_guard`.
- **A default-bridge pick pairs back to the declaration whose realization it is.** ktor's
  `HttpClient(CIO)`: `fun <T : HttpClientEngineConfig> HttpClient(engineFactory:
  HttpClientEngineFactory<T>, block: HttpClientConfig<T>.() -> Unit = {})` called with the
  defaulted lambda omitted. The top-level picker resolves such a call through the `$default`
  bridge (`select_top_level_default_callable`, which infers `T` and marks the callable
  `default_call`), and hands the bridge back without a selected candidate; the signature-pass
  wrapper (`select_top_level_function_candidates_with_visibility`) then paired the bridge to a
  base candidate by NAME and DESCRIPTOR — but the bridge carries its realization's coordinates
  (`HttpClient$default`, the bridge descriptor), so the pairing failed for every omitted-default
  classpath call in Pass 1, and `HttpClient(CIO)` fell through to the class constructor and
  declined the property. Every `val client = HttpClient(CIO)` in a real project lost its inferred
  type and each use cascaded (`cannot infer the type of property`, `unresolved reference` on the
  client's members). The wrapper now also pairs a `default_call` callable to the candidate whose
  `default_realization` names it. (Ranking the omitted shape in the picker instead was tried and
  rejected: it bypasses the bridge, and the emitted call must target `name$default` with its
  mask.) Tests: `tests/omitted_default_generic_overload_e2e.rs` (the shape as a krusty-compiled
  library with both facades and the class; checked and run),
  `streaming_signature_tests::a_generic_call_omitting_a_defaulted_trailing_parameter_selects_in_pass_one`.
- **A signature-pass member call binds a result-only formal from the parameter it fills.** mockk's
  `any()` is a generic member of the lambda's receiver scope (`fun <T : Any> any(): T`) whose `T` is
  fixed only by the parameter it fills: `coEvery { repo.listReapable(any()) }` inside
  `= runTest { … }` selects `listReapable(cutoff: Instant)` with `T := Instant`. The member
  expectation phase (above) already hands `any()` that parameter, but the paths that finish a
  selected member — the demanded-signature path (`apply_demanded_member`) and the implicit-receiver
  member return — ignored it and returned the open `T`, so the enclosing call reported `none of the
  following candidates is applicable` on every such stub (eight in one real test source set,
  more in two others). Both paths now unify the selected result with the expectation for formals
  the arguments left open, as the source-callable path already did; an explicit receiver
  (`scope.any()`) reaches the same rule because a member-call argument now counts as a contextual
  call and the expectation-aware evaluator has a member-call arm. Test:
  `streaming_signature_tests::an_implicit_receiver_generic_member_binds_its_result_from_the_parameter_it_fills`
  (implicit and explicit receivers, three parameter shapes).
- **Three more flow facts reach the signature pass (and one the body check).** The body check
  narrows a stable value after `x!!`, after `assertTrue(x != null)` (kotlin.test's `returns()
  implies actual`), and on the true edge of `if (x?.p != null)`; the compact model an inferred
  signature is solved on knew only `if (x != null)` and, since the contract rule above, a plain
  `requireNotNull(x)`. Test bodies written as `= runTest { … }` lost their signature to each of
  these: `assertEquals(RUNNING, result!!.status); assertEquals("unknown", result.host)` and
  `assertTrue(result != null, msg); result.contains(…)` (six reads in one real test source set),
  and `if (session?.mock == true) { mockHttpClient(session) … }` in an inferred-signature lambda
  (six more in a main set). The extractor now (1) rebinds every name asserted `x!!` in a statement
  — outside lambdas — to its non-null value for the rest of the block, including the trailing
  expression; (2) takes an argument spelled `x != null` as a contract candidate whose proof is a
  `returns() implies actual` effect (`ContractNarrowed { condition: true }`); (3) narrows the
  safe-call receiver of `x?.p == literal` / `x?.p != null` on the true edge of an `if`. The body
  check gained the `x?.p == literal` form too (it had `!= null` only), through the same
  `null_check_narrowings` proof. The `x!!` and `assertTrue` shapes are byte-identical to kotlinc;
  the two safe-call comparisons carry a pre-existing emission residue (krusty boxes the safe-call
  result through `Boolean.valueOf`, a temp and a `checkcast` where kotlinc compares the unboxed
  value under a `dup; ifnull`), untouched by the narrowing. Test:
  `streaming_signature_tests::expression_body_narrows_after_a_not_null_assertion_a_boolean_contract_and_a_safe_call_test`,
  `not_null_assert_e2e::inferred_signature_flow_facts_round_trip_through_a_dependency`
  (default-on kotlinc dependency cross-check).
- **A Java-origin mutable collection expectation binds a read-only generic result.** A Java
  `Map<K, V>` reaches Kotlin as the flexible `(Mutable)Map<K!, V!>!`; krusty carries its mutable
  face under platform nullability (`MutableMap<String!, Any!>!`). Wherever that face is the
  EXPECTATION for a generic call's result — a `T` bound to it and handed on (`every { j.attributes }
  returns emptyMap()`, mockk's everyday stub of a Java getter) — the result variables must still
  bind, as kotlinc binds them through the read-only side of the flexible type. The return
  unifiers (`unify_inferred_ty`, `unify_ty`, and the scorer's restatement of the declared return
  at the expected constructor) now ask the declaration provider for the resolved upper face of a
  platform-flexible expectation. The JVM provider answers from its id-backed Java/Kotlin class map;
  common inference contains no collection-name table. A Kotlin-declared `MutableMap<String, Any>`
  carries no platform mark and still rejects `Map<K, V>`, as kotlinc rejects it. Tests:
  `tests/java_flexible_collection_e2e.rs`.
- **A lambda whose input is still an open callee formal is a postponed probe.** A generic call
  whose lambda parameter's INPUT is one of the callee's own formals (`fun <T : Any> matching(f:
  (T) -> Boolean): T`, mockk's `match { it.contains(x) }`) can only shape that lambda once `T` is
  known. In argument position the enclosing call supplies `T` after it selects, and krusty already
  re-checks the nested call under that expectation — but the first applicability probe checked the
  body against the open `T` and REPORTED what it found (`unresolved reference 'contains'` on `it`,
  once per matcher in a real project's `verify { … }` blocks). kotlinc postpones such a lambda. The
  shared lambda entry point (`check_lambda_with_implicit_receivers_and_return_labeled`) now moves
  the body diagnostics of a lambda whose inputs are not lexically fixed into an ordered postponed
  buffer, outside the shared diagnostic sink. A closed recheck discards that expression's buffered
  probe; statement completion publishes any probe nothing superseded. The top-level path's
  `deferred_member_errors` follows the same ownership rule. A statement call whose result still
  has a result formal without a retained binding is now diagnosed from its `GenericSig`, resolved
  type arguments, and unsolved PCLA inputs for every callable origin, replacing the
  classifier-value-only tracking set; `scope.m { it.foo() }`
  therefore reports both the uninferable `T` and the unresolved body member. Test:
  `tests/postponed_lambda_probe_e2e.rs`.
- **A packed array is built through a local, never a `dup` chain.** kotlinc 2.4.10 emits every
  packed array — a vararg call's elements, `arrayOf`, `intArrayOf`, `listOf(...)` alike, in static and
  instance bodies — as `anewarray; astore n; aload n; iconst_0; <e0>; aastore; …; aload n`, with `n`
  the next free local at that point, released again afterwards. krusty used `dup; index; <element>;
  aastore`, one instruction shorter per element and different on every vararg call site in a real
  module. The `Vararg` emitter now spills the fresh array into `next_slot` (registered in the frame
  slot map while the elements are emitted, so a branchy element frames it as live), reloads it for
  each store and for the final use, and releases the slot when it is the topmost one. Measured
  byte-identical for a top-level `f("a", "b")`, a non-final vararg with a trailing named argument, an
  instance method, `arrayOf`/`intArrayOf`/`listOf`, and a parameter used as an element. Still open
  beside it: kotlinc allocates a `val`'s slot BEFORE evaluating its initializer (`val t = f("a")`
  gives `t` slot 0 and the packing temp slot 1), and it `checkcast`s a `String` receiver to a
  `CharSequence` EXTENSION receiver (`"a".split(...)`), neither of which this rule covers. Test:
  `tests/vararg_elements_before_named_e2e.rs` (`packed_vararg_arrays_are_byte_identical`,
  `module_vararg_elements_before_named_argument_are_byte_identical`).
- **Two parser shapes that dropped whole files from a real project's test source sets.** A parse
  error removes every declaration in the file, and each sibling that names one of them then fails
  with `unresolved reference` — one bad line cost 38 and 49 errors respectively. (1) An enum entry
  list closed by a trailing comma and a `;` on its own line, then a member: `HTTP(...), TCP(...),`
  newline `;` newline `companion object { … }`. The `;` is lexed as a newline, so the entry loop
  read `companion` as the next entry and failed on `object` ("expected object name"). An entry name
  must be followed by `(`, `{`, `,`, a separator, or `}`; anything else starts the members.
  (2) `target op= value` whose target is not assignable — `m[k]!! += x`, `(m[k]!!) += x` — is
  Kotlin's operator-call form (`plusAssign` on the value), not an assignment; the parser rejected
  everything but a name, an index, a member, or a call ("invalid assignment target"). It now hands
  any other target to the same `CompoundAssign` statement the call form uses, and the checker
  decides whether the value's type offers the operator. Tests: `tests/test_set_parser_gaps_e2e.rs`
  (diagnostics plus a box run for each).
- **Signature-pass member selection sees only the overloads the written arguments can fit.** The
  Pass-1 solver hands selection one argument per parameter SLOT, an omitted defaulted slot as
  `OmittedDefault`. A sibling overload with NO default at that slot then looked exactly as
  applicable, so `Instant.fromEpochSeconds(0)` — `(Long, Int = 0)` beside `(Long, Long)` — was
  "ambiguous" in signature position (an inferred-return `fun at() = Instant.fromEpochSeconds(0)`)
  while the same call in a body, mapped per candidate, selected. The member path now keeps only the
  overloads whose own names/defaults/vararg shape can consume the written arguments, as the
  top-level and extension paths already did. Side effect measured by an existing test: a provider
  member taking a callable reference (`HashMap.merge("a", 2, Int::plus)`) now selects in Pass 1
  instead of declining. Test: `tests/test_set_parser_gaps_e2e.rs`
  (`an_int_literal_selects_a_long_parameter_beside_a_defaulted_one`).
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
- **A non-local `return` out of a lambda spliced into a classpath `inline fun` boxes like any other
  suspend return.** Inside a lambda's inline template every surviving `IrExpr::Return` already crosses
  out of the lambda (a local `return@label` became a labelled exit at template preparation), so the
  splice realizes it as a return from the ENCLOSING method — and in a CPS body that method returns
  `Object`. `box_returns` used to stop at `Lambda`, leaving `bipush 100; areturn` (VerifyError: `Bad type
  on operand stack`) and a void `return` where a value is expected; it now walks the lambda's
  `inline_body` too. A plain function returning `Any` was never affected: its coercion is inserted by
  the lowering. The invariant is scoped to returns that leave the template being spliced: a
  `return@outer` inside a lambda nested in ANOTHER emit-time-spliced lambda is prepared only by the
  inner template and survives as a raw `Return` the emitter realizes as the method's — a pre-existing
  gap this rule does not close. Test: `tests/suspend_inline_splice_nonlocal_return_e2e.rs`.
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
  **An `invokedynamic` relocates with its whole bootstrap entry, and only if that entry may move.**
  The instruction names a `BootstrapMethods` entry of its DEFINING class by index, not a pool entry,
  so relocation re-interns the entry — its method handle, its static arguments and its name/type —
  in the host (`ClassWriter::add_bootstrap` dedupes). Whether it may move is decided from the
  entry's dependency graph, never from the factory's spelling: the relocation inventory reports
  every member and class the handle, its descriptors, its static arguments, and the call-site
  descriptor reach, `None` for a
  constant kind or descriptor relocation cannot carry (`CONSTANT_Dynamic`, a handle onto a
  non-member, an index past the pool), and `references_private_member` refuses a splice unless each
  bootstrap dependency is provably public — a stricter question than it asks of an ordinary
  instruction operand, because bootstrap linkage has no verifier-visible use site. That is
  what separates a `StringConcatFactory` entry (a public factory, a recipe string, constants) from a
  `LambdaMetafactory` one (an implementation handle in the declaring class, usually private and
  synthetic), without either name appearing in the rule. An inaccessible entry that relocated would
  throw `BootstrapMethodError` when its instruction first executes — after verification, so only a
  RUN observes it: `classpath_inline_splice_e2e::the_relocated_concatenation_bootstrap_links_and_runs`
  executes the spliced concatenation for that reason, beside the emitted-form assertions.
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
  same JVM owner, and member names stay in SOURCE terms while the provider attaches the exact physical
  owner/name/descriptor realization (`removeAt` → `remove`). No filter subtracts from the Java scope and
  no reverse table exists — the correct set is simply the declared one. The OVERRIDE direction is
  unchanged: a class realizing `MutableList` writes `removeAt`, and the resolved override edge carries
  the external declaration's `remove(int)` realization into bridge derivation. Tests:
  `tests/mapped_collection_scope_e2e.rs`, corpus
  `specialBuiltins/irrelevantRemoveAtOverride.kt`.

  A CONCRETE `java.util` class (`ArrayList`, `AbstractList`) is the other half, and needs the other
  mechanism: it has a real class file, so it never consults the builtins and keeps its Java member scope —
  including its own `remove(int)`. kotlinc handles exactly this in `LazyJavaClassMemberScope`
  (`isVisibleAsFunction` / `doesOverrideRenamedBuiltins` / `createRenamedCopy`): a Java method whose
  signature matches a renamed builtin is hidden under its JVM name and re-exposed under the Kotlin one.
  krusty derives this read-side rename from the same provider-normalized builtin declaration used by
  bridge emission. The selected mapping must match the JVM name AND full erased descriptor (only
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

  Mapped collection scopes also admit the exact physical signatures in the provider-owned,
  versioned `visible_methods_2_4.tsv` policy (verified identical for the supported Kotlin 2.4.0 and
  2.4.10 toolchains), matching
  `JvmBuiltInsSignatures.VISIBLE_METHOD_SIGNATURES`. Read-only signatures such as `stream` and
  `getOrDefault` attach to the read-only declaration and are inherited by its mutable sibling;
  mutating signatures such as `removeIf`, `computeIfAbsent`, and `merge` attach directly to the
  `Mutable*` declaration. Tests:
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

- **A delegated implementation beats a super-interface's redeclared default.** `class Impl : Base2,
  Base by Delegate()` where `interface Base2 : Base` redeclares `test()` with a body answers the
  DELEGATE, not `Base2`'s default: the forwarder synthesized for `by Delegate()` is an
  implementation the class supplies, and an implementation always wins over an inherited default.
  `Base` and `Base2` name the SAME member, so a dispatch model that numbers members per declaring
  classifier has to number both spellings together — giving `Base2.test` a number of its own makes
  a class that registered its forwarder under `Base.test` look like it supplies nothing, and it
  silently takes the default. krusty's native target numbers interface members program-wide and
  groups every spelling of one vtable entry into a single number for exactly this reason; the JVM
  target gets it from `invokeinterface`. Corpus:
  `codegen/box/delegation/hiddenSuperOverrideIn1.0.kt`. Tests:
  `tests/native_delegation_e2e.rs::a_delegated_member_beats_a_redeclaring_interfaces_default`,
  `…::an_anonymous_object_delegates_one_of_its_supertypes`.

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

- **A delegated property crosses the scalar/reference boundary at FOUR places, and each was
  emitting unverifiable bytecode.** Found by bucketing the box corpus, not by reading the code; box
  total 6333 → 6344.
  - The DELEGATE reaches the operator's receiver slot. A delegate is stored at its own type, so
    `val s: String by impl` (an `Int`) pushed a raw `int` where `operator fun Any?.getValue(…)`
    declares `Object` — `Type integer … is not assignable to 'java/lang/Object'`. It is boxed
    against the DECLARED slot, not unconditionally: `operator fun Int.getValue(…)` keeps its
    receiver unboxed, and boxing every scalar delegate would break that.
  - The RESULT reaches the accessor's own return. The accessor returns the PROPERTY's type while
    the operator returns the DECLARATION's, so `val age: Int by map` returned the `Object` that
    `Map<K, out V>.getValue` leaves on the stack from `getAge()I`. The coercion goes on the
    accessor body, which is the boundary the accessor owns and where the emitter reads the value's
    own physical type — so a matching type costs nothing, a reference result gets its cast, and a
    scalar one its unbox. It is skipped where the delegated call already coerced to that same
    type, so the non-external arms keep their single coercion node
    (`fir_lower::tests::generic_member_delegate_result_keeps_its_erased_call_boundary` asserts
    exactly one). Putting it on the delegated CALL instead (coercing the external target's
    declared result to the selected one) fixed the reference case and not the scalar one, because
    a stdlib `getValue` is spliced rather than called.
  - The DELEGATE EXPRESSION reaches `provideDelegate`'s receiver slot, which is the same boundary
    one phase earlier: `val byInt by 42` pushed a raw `int` into
    `provideDelegate(Object, Object, KProperty)`. The adaptation fact comes from the CHECKED
    delegate call — `FirDelegateCall` now carries the call's own `receiver` and the selected
    callable's `declared_receiver` — because the caller had nothing to compare against for a
    `provideDelegate`, which is what left this hole after the accessor receiver was fixed (box
    `delegatedProperty/provideDelegate/genericProvideDelegateOnNumberLiteral.kt`).
  - The WRITTEN VALUE reaches the operator's declared parameter: `var x: Long by …` handed a raw
    `long` to `setValue(Object, Object, Object)`. Arguments are adapted against the slots the
    callable DECLARES, tail-aligned with its parameter list (a callable's own value parameters
    follow any context parameters), so a `setValue(…, newValue: Long)` keeps the value unboxed and
    an extension operator on a scalar owner gets its `thisRef` boxed for the same reason. Only the
    CROSS-FILE shape was broken — a same-file operator is a method of the file's own IR and the
    write reached it already adapted (box
    `delegatedProperty/genericSetValueViaSyntheticAccessor.kt`, whose operator is `protected` and
    inherited; that case now advances past the `VerifyError` to the separate
    super-constructor-argument boxing gap below).

  The slots each value reaches come from the CHECKED PLAN, not from a signature read back in
  lowering. `FirDelegateCall` publishes the operator's applied value parameters and the slots the
  declaration spells for the same call, in the same order and always the same length. The declared
  side is UN-ERASED — `setValue(…, newValue: T)` reads `T`, not `Object` — because what a `T` slot
  costs a value is a target's answer and differs between targets; lowering states only that two
  semantic types differ. The type of the `KProperty` operand is published the same way, so
  resolution's applicability classifier and the value lowering builds are one answer. A plan
  whose lengths disagree is a broken contract between two phases and fails as
  `InvalidDelegatedCallShape`; it never lets an argument through unadapted, which is how a raw
  `long` reached an `Object` slot in the first place. There is no prefix for lowering to find the
  end of: a CONTEXT-PREFIXED convention operator is not a delegate convention at all, because the
  operator is called from a generated accessor with no scope to fill an implicit context from.
  kotlinc rejects such a declaration outright ("context parameters on delegation operators are
  unsupported") and then reports the property as having no applicable `getValue`; krusty emits the
  same declaration diagnostic, from the SAME rule that excludes the candidate during selection, so
  the two answers cannot drift apart, and on the same `context(…)` clause kotlinc anchors it on.

  A property whose delegate supplies no convention is reported by the FRONT END, never as a checked-
  FIR failure: that is an ordinary source mistake, and an internal error is not a diagnostic. The
  report is anchored on the `by` keyword, which is what kotlinc anchors it on, and it is made once
  per convention the property needs — a `var` is told about `getValue` AND `setValue`, even though
  the first already failed. kotlinc has two shapes for it and krusty reproduces both: with no
  function of that name in reach, `type 'Plain' has no method 'getValue(Holder,
  KMutableProperty1<*, *>)', so it cannot serve as a delegate.` (and `… for var (read-write
  property).` for `setValue`); with functions of that name that are none of them applicable,
  `property delegate must have a '…' method. None of the following functions is applicable:` and
  the candidates, each rendered with its context prefix, its parameter names and its result. The
  candidate list is every function of that name the delegate's scope offers, whatever excluded it —
  an inapplicable overload, a missing `operator` modifier and a context prefix alike, because each
  is a thing the author plausibly meant to be the convention. The second slot of the demanded
  signature is the property reference the accessors would pass: `KProperty`/`KMutableProperty` by
  mutability, numbered by receiver count (none, member or extension, member extension), and
  star-projected UNLESS exactly one candidate is to blame, which is when kotlinc names the
  receivers and the property type outright. `thisRef` follows the same shape, printing `Nothing?`
  where the accessor passes null. For an inferred property this same convention report belongs to
  signature finalization: the compact delegate site retains its declaration kind, mutability,
  receivers and exact `by` origin, so a failed convention carries a source diagnostic into recovery
  and cannot suppress independent body errors in the rest of the file.

  All four are stated the SAME way, and it is a SEMANTIC statement: lowering compares the two
  checked types and, where they differ, records one `ImplicitCoercion` to the one the other side
  declares. What that costs — a box, an unbox, a widening, a `checkcast`, a value class's own
  `box-impl`/`unbox-impl`, or no instruction — is read off the PHYSICAL types by the backend when
  it emits the coercion. Asking `Ty::is_jvm_scalar()` in lowering instead would put a
  representation choice in the wrong phase and still leave the backend to re-derive it; it also
  gets the cases wrong that are references without being scalars, which is every nullable carrier
  (`var x: Int? by …` must pass through untouched) and every value class (`var id: Id by …` must
  cross through `Id.box-impl`, not `Integer.valueOf`). Both are pinned against kotlinc.

  The accessor's own return coercion is likewise stated once, by the CALLER, which knows whether
  the value in hand is a source-written body (already the property's type) or the checked result of
  a delegate operator (the declaration's). Reading it back off the generated node's shape guessed
  wrong in both directions — a missing coercion does not verify, a duplicated one wraps a coercion
  in a coercion.

  Still open next door, and NOT part of this boundary: a generic SUPER-CONSTRUCTOR argument
  (`class C : PVar<Long>(42L)` calls `PVar.<init>(Object)` with a raw `long`, so the class links to
  a `<init>(long)` that does not exist — a `NoSuchMethodError`, not a `VerifyError`).

  Recorded gaps the same ledgers make visible, all outside this boundary and none affecting the
  adaptation instructions: a `var`'s delegate `KProperty` is a `PropertyReference*Impl` where
  kotlinc uses `MutablePropertyReference*Impl`; a member-extension delegate's reference names the
  EXTENSION receiver's class where kotlinc names the owner; the reference's signature string omits
  a value class accessor's mangled name and carries the boxed return (`getId()LId;` where kotlinc
  writes `getId-eEFUqEU()I`); and a non-null reference setter parameter is not
  `checkNotNullParameter`-checked.

  Tests: `tests/delegate_scalar_boundary_e2e.rs` (cases covering top-level, member,
  member-extension, `provideDelegate`, cross-file and classpath operators, nullable carriers and
  both value-class carrier kinds; each either RUN or pinned instruction-for-instruction against
  kotlinc), `fir_lower::tests::a_delegated_accessor_result_crosses_exactly_one_coercion`.
  The lowering lives in `src/fir_lower/delegated_properties.rs`.

- **A delegate convention resolves `kotlin.reflect.KProperty`; it does not assume it.** The operand
  type the `getValue`/`setValue` lookup passes is obtained from the symbol source that answers
  applicability, so a dependency set declaring no `KProperty` reports the ordinary convention failure
  instead of selecting against a classifier name that denotes nothing. Because the `by` clause is a
  LANGUAGE construct, `EmptySymbolSource` publishes the declaration the way it already publishes
  `Enum` and `Function`: a target with no stdlib artifact still has it. Tests:
  `streaming_signature_bridge::delegates::tests::a_dependency_set_without_kproperty_refuses_the_convention_instead_of_assuming_it`
  (the same source accepted with the declaration present, refused without it) and the delegate
  ledgers in `tests/delegate_scalar_boundary_e2e.rs`.

- **Signature finalization names the convention it could not find, and does not suppress the file.**
  A delegated property with NO declared type has nothing to infer its type from once `getValue` is
  missing, so its signature cannot finalize — and the body check that would have reported it never
  runs for that declaration. Declining silently made finalization fail with no cause named, which
  skipped body checking for the WHOLE file: the file then reported nothing at all, its unrelated
  diagnostics included. `select_delegate_signature` records the refusal instead, in the same wording
  the checker uses, so the two collapse wherever both reach the sink. Three rules the differential
  pinned:

  * it points at the `by` keyword, as kotlinc does, so the delegate operation carries its own origin
    rather than the delegate expression's (`by` is column 13 where `Plain()` is column 16);
  * a candidate whose own return is still undetermined is RESOLVED before it is rendered, through
    the same `demand` a selected convention's result goes through. Rendering `<not determined>`
    produced a second, differently worded message for one mistake once the body check reported it;
  * with exactly one candidate to blame and no declared type, the reference names that candidate's
    result — `getValue(Nothing?, KProperty0<Int>)`, not `KProperty0<*>` — which is the type kotlinc
    reports the property as having.

  Measured divergence, pinned by both complete ordered ledgers: for `var untyped by Plain()` kotlinc
  reports THREE errors and krusty two. kotlinc cascades a second `setValue` refusal whose value slot
  renders the failed inference itself (`??? (Unresolved name: getValue)`); krusty suppresses every
  delegate-convention message whose operand types are already errors, which is what stops one failure
  being repeated under a second heading. Both compilers report the missing `getValue` at the `by`
  keyword and the file's unrelated diagnostic. Test:
  `delegate_scalar_boundary_e2e::an_untyped_delegated_property_names_its_missing_convention_and_the_rest_of_the_file`.

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
- **Signature-pass argument mapping keeps the slot-map contract, including vararg EXTRAS.** The
  Pass-1 signature solver (`streaming_signature_bridge::candidate_call_slots`) maps a call's written
  arguments onto parameter slots through the same `map_call_args` the checker uses. That mapper keeps
  ONE source per slot: positional vararg elements beyond the first stay unmapped and are the vararg's
  extras, reconstructed by lowering. The solver used to reject any candidate with an unmapped source,
  so `"a,b;c".split(",", ";", limit = 2)` and `option("--target", "-t", help = "…")` (two elements,
  then a named argument) declined in signature position while the same call in a body checked fine.
  An unmapped positional argument is now admitted as a vararg extra when it carries the SAME element
  type as the mapped first element (a spread contributes its array's element type; lambdas and
  postponed arguments never qualify) — the per-slot selection downstream sees only the mapped element,
  so an extra of another type keeps the candidate inapplicable. Argument mappings are collected only from the normalized source-callable
  family: public declarations, must-inline declarations, and stable declarations in this compilation;
  declaration origin is not inspected. Separately, a source top-level
  function's vararg need not be LAST (`fun option(vararg names: String, help: String = "")`);
  top-level selection expanded varargs with the final-slot assumption and declined every call shape
  but named-only, so it now expands at the candidate's recorded slot (`candidate_vararg_shape` →
  `vararg_parameter_shape_at`, defaults sliced past the context parameters). Tests:
  `tests/vararg_elements_before_named_e2e.rs`.
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
    mapped `java.util` spelling (`size`, `keySet`, `entrySet`) from the same exact, declaration-owned
    mapped-builtin realization policy the member table uses — one definition, so a call and a
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
  `size`/`keys`/`values`/`entries`. The builtin provider records the source name and exact JVM
  realization separately, so a call through a `MutableList` receiver invokes `remove(I)Object`, and a
  class implementing `MutableList` gets the same bridge from its resolved override edge — needed when
  the override is inherited from a NON-collection supertype, which is the only place the two names can
  diverge. Unlike the `size` entry beside it, this
  one is keyed on the KOTLIN declaration identity `kotlin/collections/MutableList` plus its full erased
  descriptor, not merely the erased `java/util/List` owner:
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

- **`expect`/`actual` requires the multiplatform feature, and an `expect` declaration may not carry
  a body.** Two independent checks, both syntactic and both measured against the reference
  compiler. (1) Without `+MultiPlatformProjects`, every `expect`/`actual` MODIFIER is an error
  (`'expect' and 'actual' declarations can be used only in multiplatform projects. Learn more about
  Kotlin Multiplatform: https://kotl.in/multiplatform-setup`), reported at the keyword, members
  included; the sentence does not vary with which modifier was written. Accepting it emitted an
  artifact that could not link — a call to an unmatched `expect fun` was written as an
  `invokestatic` of a method the facade does not declare. (2) An `expect` declaration that carries
  an implementation is an error regardless of the feature, so a file without the feature gets BOTH
  sentences, the gate first. `expected declaration cannot have a body.` covers a function with an
  expression or block body, a property ACCESSOR with a body, an `init` block, and any of these on a
  member of an `expect` classifier; `expected property cannot have an initializer.` covers an
  initializer; `expected property cannot be delegated.` covers a `by` delegate. Positions are
  measured, not derived: a top-level declaration is reported at its
  `expect` keyword, a member at its own declaration, an accessor at its header (`get()` / `set(v)`,
  hence `PropDecl::getter_span` and `PropAccessor::span`), an `init` block at the KEYWORD (hence
  `File::init_block_keywords` — the block expression's own span starts at `{`), and a property
  initializer under the INITIALIZER EXPRESSION (`expect val x: Int = 3` → column 31), and a
  delegate under the DELEGATE EXPRESSION (`by lazy { 1 }` → under `lazy`). A secondary
  constructor with a body inside an `expect class` is NOT reported, though an `init` block in the
  same position is. Once any body error exists the reference compiler never reaches actualization,
  so the unmatched-expect report is suppressed for the WHOLE compilation, not per file (measured
  with two files: a body error in one silenced a clean unmatched `expect` in the other). An
  unmatched `expect` with the feature ON reports `expected <name> has no actual declaration in
  module <name> for JVM` at the `expect` keyword, naming the `-module-name` it looked in. Tests:
  `mpp_requires_the_feature_e2e`, `expect_declaration_body_e2e`.

- **An `actual` with no `expect` to actualize is an error, named by a declaration renderer.** With
  `+MultiPlatformProjects` on, a top-level `actual` that actualizes nothing reports
  `'<rendered declaration>' has no corresponding expected declaration` at the declaration's NAME
  (`actual fun simple(): Int = 1` → column 12, under `simple`). krusty used to accept it and emit.
  The message names the declaration the way the reference compiler's own renderer does — the full
  measured grammar is in `docs/PARITY_PROTOCOL.md` — and that rendering is a HYBRID by necessity:
  source syntax owns what the declaration WROTE (kind, name, parameter names, which parameter
  carries `vararg` or a default, and a classifier's modifier words), while resolution owns every
  TYPE, because an inferred return (`actual fun f() = 1` → `Int`) and a supertype named through a
  typealias (`class ViaAlias : AliasBase()` → `: Base`) have no written form to copy. The two
  halves are read at different TIMES: which `actual` is unmatched is a source-set question,
  answerable only while every file's syntax is live, and the rendering is not built until Pass-1
  signature finalization has published the types it names. Three resolved facts each need a
  correction the naive read gets wrong: a generic callable's `Signature::params`/`ret` are ERASED
  (its declared shape is on `GenericSig`), a `vararg` parameter's declared type is the ARRAY it
  arrives as while the rendering names the ELEMENT, and a classifier's type parameters are stored
  as semantic identities whose source spelling is what gets rendered. The modifier words a
  declaration wrote are rendered in the reference compiler's measured order —
  `external override inline operator infix tailrec suspend` for a callable,
  `external const lateinit` for a property, `inner data value fun` for a classifier — and omitting
  them silently dropped `const val`, `lateinit var`, `operator fun` and `external fun` from the
  message until the fixture was widened to include them. An unmatched `actual` is
  found by ACTUALIZATION ITSELF, not by a name/arity key: that matcher compares resolved type
  shapes and follows an `actual typealias`, so it pairs `expect val S.tag: S` with
  `actual val String.tag: String` where a key differing on the receiver spelling cannot (reporting
  those was a real regression the harness caught). Actualization's pairing is the ONLY
  authority: a name/arity key differs on the receiver spelling exactly where actualization follows
  an `actual typealias`, so consulting it as a second answer reported pairs that had matched. An
  `actual typealias` publishes a type EXPANSION rather than a callable or classifier signature, so
  nothing resolved carries its identity; it is found in the compact header inventory by the
  inventory's own EXACT anchor — this file, the alias's range, no owner, the type-alias kind, and
  its position in the list a file keeps aliases in. A range alone is not an identity, because a
  constructor property and the class declaring it share one and so do a property and its accessor,
  so identity and coordinate are taken together where the declaration's syntax is read (a file
  declaration through the inventory's positional record of what it interned each parsed
  declaration as, a member through the same exact anchor under its owner) and travel together
  afterwards. A declaration this check can find no stable identity for reports an internal error
  at its own name rather than falling back to a key. A declaration with CONTEXT PARAMETERS renders them before the visibility
  slot (`context(tally: Tally) public final actual val slotted: Int`), with the names the
  declaration wrote and the types resolution published; such a property used to be passed over
  entirely.

  A MEMBER that wrote `actual` is reported at its own name by its OWN outcome, independently of
  its owner's. Actualization pairs a matched classifier's members individually
  (`actualized_declaration_pairs` walks each matched pair's children and pairs them by kind, name,
  receiver and arity), so `expect class Holder { fun kept(): Int }` with
  `actual class Holder { actual fun kept() = 1; actual fun extra() = 2 }` reports `extra` and
  nothing else — the owner and `kept` both actualized something. Measuring this needs a real
  source-set split, because the reference compiler rejects an `expect` and its `actual` in the same
  module before reaching the question; the header is passed as `-Xcommon-sources`. A member's
  modality slot is the part that is not `final`: an `override` of an `open` member renders `open`,
  and an interface member renders `abstract` without a body and `open` with one. A member's
  resolved record is selected by the STABLE DECLARATION IDENTITY the compact header inventory
  anchors on its source range — never by name and arity, which cannot tell two overloads that tie
  on arity apart and left both of a tied pair unrendered. A member EXTENSION property is a separate
  declaration in a separate table (`ClassSig::member_ext_props`), consulted by the same identity,
  and renders its receiver before the name while the diagnostic still points at the name. It also
  renders its OWN type parameters ahead of that receiver — `public final actual val <S> S.kept: S`
  declares an `S` that shadows its owner's — with the names the source WROTE, since a rendering
  shows what was written; resolution holds those formals under internal placeholder spellings and
  the bounds are read from there. A receiver written as a classifier path that the file's scope
  binds to NO classifier records that, and only that: the coarse child key says the scope bound
  nothing, and `select_actual` compares the complete type shape. A type parameter is the case this
  serves, and two of them are told apart positionally and by their DECLARED BOUNDS — a bound is
  the whole of what such a receiver says about the values it admits, so two members differing only
  there are different declarations. An unresolved or ambiguous spelling reaches the same key and is
  excluded by the comparison instead, which demands a binding on both sides. Within one
  declaration the positional map is read innermost first, so an own type parameter shadows an
  enclosing one of the same spelling and the implementation may rename it. Refusing to key such a
  A matched classifier still answers for the members it never implemented. A member actualizes by
  its own identity, so a matched owner says nothing about them, and an owner implementing none of
  them is otherwise accepted in silence; the implementation is the declaration that got it wrong,
  so it is named once at its own name — `'actual class Owed<T> : Any' has no corresponding members
  for expected class members:` — rather than each `expect` member being reported as unfilled from
  the side that did not. The owed members are listed under that line as the common source DECLARED
  them: `expect fun <S : Number> generic(s: S): S`, `expect val starred: List<*>`,
  `expect fun defaulted(a: Int = ...): Int`. Every other rendering in this check is built from a
  resolved signature and these cannot be — an `expect` subtree is excluded from the resolved model,
  so neither the members nor their classifier is published — but the listing is source text in the
  reference compiler too, so it is rendered from declaration syntax while that syntax is live. A
  property parameter on an expected class's constructor is rejected outright, so methods and body
  properties are the whole of what a classifier can owe. The listing follows a newline inside the
  same diagnostic, so the differential harness — which compares one `: error:` line each — pins the
  first line only; the listing is checked against the reference compiler directly. Test
  `no_expect_for_actual_e2e::a_classifier_owing_expected_members_is_reported`.

  An `expect`/`actual` pair must SPELL its type parameters alike, and a rename is an
  incompatibility between two declarations already taken to be counterparts — `the 'expect' and
  the 'actual' declarations are incompatible.` at the implementation — not an implementation that
  answered for nothing, and not a member its owner is left owing. A differing upper BOUND is the
  other answer: it means no counterpart was found at all, so the owner owes the member and the
  implementation corresponds to nothing. The two are told apart by asking the input-shape
  comparison twice, once requiring the names to agree and once positionally: only the second
  answering is what a rename is. Tests
  `no_expect_for_actual_e2e::a_renamed_type_parameter_is_an_incompatibility` and
  `::a_member_whose_bound_differs_is_not_a_counterpart`.

  member at all left an `expect` and an `actual` written identically pairing with nothing, and box
  `multiplatform/k2/basic/expectActualFakeOverridesWithTypeParameters.kt` regressed; tests
  `no_expect_for_actual_e2e::a_member_extension_on_its_own_type_parameter_matches`,
  `::a_renamed_own_type_parameter_receiver_matches`,
  `::a_member_extension_on_its_owners_type_parameter_matches`,
  `::a_member_extension_function_on_a_type_parameter_matches`,
  `::a_type_parameter_receiver_compares_its_bound` and
  `::a_member_extension_property_renders_its_own_formals`. A nested
  classifier and a `companion object` are hoisted out of their owner by the parser, so neither
  rides a member list: each is recorded as an actualization target of its own where its modifier
  list is read, renders its OWN simple name, and a companion renders the word `companion` before
  `object` — an edge its owner records. An anonymous `companion object` is named by its `object`
  keyword, which is where the reference compiler points. A primary-constructor property carries
  `actual` on the parameter and renders like any other `val`/`var` member. Nothing that wrote
  `actual` is passed over in silence: a member whose resolved record cannot be reached reports an
  internal error at its own name rather than disappearing.

  **Which classifier a written path names is the FILE's ordinary resolver scope, not its
  spelling.** Actualization runs before full signature solving because it decides which compact
  declaration subtree survives. Before it does, resolution binds every classifier type it may
  compare through the same module + provider scope tower used by ordinary signatures: own package,
  explicit imports and aliases, wildcard imports, Kotlin defaults and platform defaults.
  Actualization consumes only the resulting `(source, header type) -> TypeName` table; it cannot
  inspect imports, query a provider, render a name, or intern an unresolved spelling. `import
  plib.model.Tally` against `plib.model.Tally` written out is one classifier;
  `import plib.model.Tally as Ledger` puts that classifier under the name `Ledger`; `import
  plib.left.Tally` against `import plib.right.Tally` are two. A simple name more than one
  import CLAIMS is AMBIGUOUS — two wildcard imports that could each supply it, or two explicit
  imports written under it: the file has not said which classifier it means, so it names none
  and pairs with nothing. Answering with the first match, or with whichever import came last,
  would pair two declarations that name different classifiers. What is not a choice is spared:
  one classifier imported twice, or one package wildcard-imported twice, still names that
  classifier, which is why the wildcard scan counts DISTINCT classifiers rather than occurrences.
  Every answer is an interned `TypeName`, so a comparison is identity equality and a spelling is
  lookup input exactly once. An unresolved or ambiguous type has no binding and cannot match even
  another unresolved spelling. Dependency wildcard candidates participate through their provider,
  so two dependency classifiers named alike do not collapse to one bare name. Tests:
  `fir::header::actualization::tests` for each import form, the two ambiguous pairs and the
  repetitions they spare, `resolve::actualization_names::tests` for dependency-provider wildcard
  identity, and
  `no_expect_for_actual_e2e::a_star_imported_classifier_of_another_package_does_not_pair`,
  `::an_import_alias_names_the_classifier_it_renames`,
  `::a_star_import_supplies_the_classifier_it_brings_into_scope`,
  `::identical_simple_names_from_different_packages_do_not_match`.

  **The target a diagnostic names is the PLATFORM's name for itself.** `expected <name> has no
  actual declaration in module <m> for JVM` had `JVM` as a constant in the common frontend, which
  would still say `for JVM` under another backend. `SemanticPlatform::diagnostic_target_name` is
  the provider's answer, beside `external_property_diagnostic_label`; a provider that takes no
  part in source-set diagnostics answers `None`, and the check reports an internal error rather
  than inventing a name. Test:
  `mpp_requires_the_feature_e2e::an_unmatched_expect_names_the_module_it_looked_in` and
  `no_expect_for_actual_e2e::the_check_needs_the_multiplatform_feature`, which runs the reference
  compiler without `-Xmulti-platform` too and compares complete ledgers.

  **What a declaration resolved to is published by RESOLUTION, once.**
  `resolve::declaration_index` walks each published table one time and enters each record under
  the identity it already carries; every reader does lookups. A reader that rescanned `funs`,
  `ext_funs`, `source_props`, `ext_props` and four more tables per classifier depended on how many
  tables a declaration may appear in and on which order to try them. Two records claiming one
  identity is a contract failure recorded as a conflict, not a last-writer-wins `insert`: the
  check reports an internal error at the declaration rather than rendering whichever arrived
  second.

  **An implementation is a declaration that WROTE `actual`.** A declaration sharing an `expect`'s
  package, kind, name, receiver and arity is the implementation that header was written for, and
  the reference compiler says so AT THE DECLARATION — `declaration must be marked with 'actual'.`
  at its own name — while leaving the header matched. Accepting such a declaration as a valid
  implementation instead let it suppress the unmatched-`expect` error, exclude the `expect`
  subtree and inherit the header's defaults from a coincidence; reporting the header as
  unactualized instead would name the same mismatch from the side that did not get it wrong. The
  modifier is unrecoverable afterwards — it is gone by the time headers are compacted, and a
  declaration's shape says nothing about what it claimed — so the parser's record of it is
  published as `DeclarationFlags::ACTUAL`.
  (`an_ordinary_declaration_of_the_same_shape_does_not_actualize`.)

  **What a pair compares is a classifier IDENTITY, not a spelling.** Actualization runs before
  signatures are resolved, so the identity is the one each file's package and imports establish
  over the classifiers the module declares: `plib.model.Tally` written out and `Tally` under
  `import plib.model.Tally` are one classifier, and `Tally` imported from `plib.left` and from
  `plib.right` are two. A path nothing claims keeps its own spelling, which is a canonical form
  both sides reach the same way rather than a fallback to the text. A TYPE PARAMETER is matched by
  POSITION, the owners' before the declaration's own — `expect class A<B, C> { fun o(b: B): C }`
  against `actual class A<C, B> { actual fun o(b: C): B }` is one legal pair, and a member that
  started from an empty scope could not see it. An `actual typealias` is followed by the QUALIFIED
  identity of the `expect class` it actualizes, in the member-extension receiver key as well as in
  the type comparison. There is no lone-candidate fallback: every kind answers — a callable, a
  constructor and a property by their input shapes, and the kinds that declare no inputs by the
  coarse key that bucketed them — where accepting a single coarse candidate paired declarations
  whose shapes had already been compared and rejected.
  (`an_imported_and_a_qualified_spelling_of_one_classifier_match`,
  `identical_simple_names_from_different_packages_do_not_match`,
  `a_member_extension_on_an_actualized_alias_matches`.)

  **Syntax meets resolution at ONE identity.** The check holds parser declarations — names,
  modifiers, spans — and has to find what each resolved to. It asks the compact header inventory
  for the identity it anchored on that declaration's own range, through a map the inventory
  publishes once, rather than walking the stub list comparing ranges; and it asks one published
  index for the resolved record, rather than entering a name-keyed table and filtering what comes
  back. Every member table a classifier keeps — its methods, its member extension functions, its
  declared properties, its member extension properties — is in that index, so a name shared
  between two of them cannot answer with the wrong record, because no name is consulted. A
  `typealias` is the one declaration whose resolved expansion is keyed by a qualified TYPE name
  rather than by a declaration; that name is published once from the inventory beside the alias's
  declaration identity, so a reader asks by the declaration it holds.

  A file's `actual typealias`es are reported where the SOURCE writes them: they live in their own
  parser list, and a report that emptied one list after the other put every alias last however the
  source interleaved them. (`an_alias_is_reported_where_the_source_writes_it`.)

  Note the deliberate model
  difference this check makes visible: the reference compiler rejects an `expect` and its `actual`
  in the same module, while krusty compiles a platform module and its `dependsOn` chain as one
  source set, so a pair in one file is matched here and unmatched there. Tests:
  `no_expect_for_actual_e2e` — differential against the reference compiler on the same source,
  because a rendering this detailed is exactly what a transcription gets wrong, and comparing each
  compiler's COMPLETE ordered ledger of errors: a comparison that keeps only this check's own
  sentence cannot see a second diagnostic either compiler started or stopped reporting.

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

- **A `Unit` call before a bare `return` is in tail position.** In a `Unit`-returning `tailrec`
  function the tail call is written as a statement, and Kotlin still counts it as a tail call when
  the only thing after it is `return` — `f(x); return` and `if (c) { f(x) }; return` both cost no
  stack. The checked lowering rewrites the statement immediately before a trailing bare `return`,
  and only that one: anything earlier has code after it and stays an ordinary call, which is what
  the corpus marks `NON_TAIL_RECURSIVE_CALL`. Descent is into blocks, `when` branches and returns
  alone, so a self-call guarded by a loop or a `try` is left recursive
  (`src/fir_lower/tailrec.rs`,
  `tests/native_codegen_e2e.rs::a_unit_tail_call_before_a_bare_return_becomes_a_loop`).

- **A `tailrec` the rewrite declines is declined by the backend too, not compiled.** `tailrec` is a
  promise about stack, and the sources that use it recurse past any stack, so emitting the recursion
  means emitting a program that dies on a guard page where it should print its answer — and whether
  it dies depends on the machine's stack limit, which makes a gate that accepts it unreproducible.
  Since the rewrite became a question about the FRAME rather than about where a function was
  declared (see "What `tailrec` loops is a FRAME" below), a member and an extension are rewritten
  like any top-level function, and what remains recursive is narrower: a context parameter, a local
  `tailrec`, an OVERRIDABLE member — which the frontend rejects outright, because looping it would
  devirtualize a virtual call — and a self-call in a position the rewrite does not descend into,
  such as under a `try`. The checked lowering records every one of them in
  `IrFile::unlooped_tailrec` and the native backend declines the file
  (`src/fir_lower/sink.rs`, `src/native/codegen/lower.rs`,
  `tests/native_codegen_e2e.rs::a_tailrec_the_checked_lowering_leaves_recursive_is_declined`).

- **A `value class` answers by the value it wraps.** `IC(1) == IC(1)` is true, `IC(1).toString()`
  is `IC(n=1)`, and the hash is the wrapped value's — where an ordinary class answers all three by
  identity. The JVM reaches that by erasing the class to its underlying value entirely; natively
  the object stays and the three members are synthesized beside it, which is the same answer by a
  different road and needs none of the JVM's mangling or box adapters. A value class that declares
  one of the three keeps its own (`src/native/classes.rs`, `src/native/codegen/lower/objects.rs`,
  `tests/native_codegen_e2e.rs::a_value_class_answers_by_the_value_it_wraps`).

- **An operand of a bounded type parameter is a box, and operators unbox it.** `T : Int` is carried
  as a reference, exactly as the JVM carries it, while Kotlin's `+` is the primitive operator. The
  operand is therefore unboxed through its bound before the two sides are unified, and the RESULT
  of the operation is that primitive rather than the reference the declaration spells — a caller
  told otherwise skips the boxing the next parameter needs. Unifying by machine type without this
  adds a pointer to an integer and prints the sum as an answer
  (`src/native/codegen/lower.rs`,
  `tests/native_codegen_e2e.rs::an_operand_of_a_bounded_type_parameter_is_unboxed`).

- **An interface member has one slot number, program-wide.** A call through an interface-typed
  value knows the interface and not the class, so the number it dispatches on has to mean the same
  member in every implementation. Each class's table is therefore its own slots, padded to a common
  base, then one entry per interface member in the program; a class fills the entries of interfaces
  it implements and the rest are the abstract trap, which nothing can name through a type it has.
  A class satisfying an interface member with a method it INHERITS (a fake override) points the
  interface's number at the inherited slot. Where the inherited method's representation differs
  from the interface's — `Raw.foo(): Int` for `Boxed.foo(): Any` — a bridge is needed and the file
  is declined instead (`src/native/classes.rs`,
  `tests/native_codegen_e2e.rs::an_interface_dispatches_through_a_program_wide_slot`,
  `::an_override_that_needs_a_bridge_is_declined`).

- **An adapted callable reference needs no adapting here.** A reference is adapted when the
  function it names does not match the type it is used as: an argument left to its default, a
  `vararg` given one element, a result discarded for a `Unit`-returning expectation. The checked
  lowering builds an adapter function for each, and that adapter is an ordinary function, so the
  reference is an ordinary function value. The decline that said otherwise was a guess about work
  someone else had already done (`src/native/codegen/lower/functions.rs`,
  `tests/native_codegen_e2e.rs::an_adapted_callable_reference_runs`).

- **Touching an enum builds all of it, then its companion.** Kotlin initializes an enum class as a
  whole: every constant in declaration order, and the companion object after — a program that only
  ever mentions `E.Y` still runs `E.init(x)` first. So one initializer per enum fills every
  constant's slot and then asks for the companion, and reading a constant, `values()`, `valueOf`
  and a call to a companion member all run it. Its flag is set BEFORE the constants are built, so a
  constant's own constructor reaching back into the enum finds the work under way rather than
  starting it again, which is what the JVM's re-entrant class initialization does. An enum's
  constructor deliberately does NOT create the companion, though every other class's does: that
  would run the companion's `init` in the middle of the first constant
  (`src/native/codegen/lower/enums.rs`,
  `tests/native_codegen_e2e.rs::an_enum_class_is_built_whole_when_it_is_touched`).

- **An enum constant's `name`, `ordinal` and `toString` come from `kotlin.Enum`'s own storage.**
  The base is the language's, declared in no file, so the two fields sit at fixed offsets ahead of
  the class's own and the runtime answers `toString` with the name — where `kotlin.Any` would
  answer with the identity (`src/native/classes.rs`, `src/native/runtime/krusty_rt.c`).

- **An exhaustive `when` needs no `else`, and gets a loud failure instead.** The frontend proves
  exhaustiveness over an enum or a sealed hierarchy; the generator does not repeat the proof, so
  the fall-through is the runtime's failure — which is what Kotlin puts there too
  (`NoWhenBranchMatchedException`). It costs a few unreachable instructions and never a wrong
  answer (`src/native/codegen/lower.rs`).

- **A secondary constructor delegates, then runs its own body.** `constructor(x) : this(x, x) { … }`
  reaches another constructor of the same class, which runs the class's initializers; a
  `super(…)`-delegating one belongs to a class with NO primary constructor, and common lowering has
  already folded that class's initializers into this constructor's body, so running them again here
  runs them twice. A delegation argument may call a companion member, so the companion is created
  first, exactly as the primary constructor creates it (`src/native/codegen/lower/objects.rs`,
  `tests/native_codegen_e2e.rs::a_class_may_have_more_than_one_constructor`).

- **A default argument is evaluated in the CALLEE's frame.** `fun f(a: Int, b: Int = a + 1)` writes
  its default in terms of a parameter, so the call site cannot compute it. Each omission shape gets
  a wrapper taking exactly the supplied arguments, which declares the callee's whole frame, fills
  the omitted slots in declaration order, and calls through — the JVM's `$default` synthetic
  answers the same problem with a bitmask because its callers may be in another compilation unit.
  A defaulted call on an open member still dispatches on its receiver: filling arguments does not
  decide which implementation runs (`src/native/codegen/lower/defaults.rs`,
  `tests/native_codegen_e2e.rs::a_call_may_leave_arguments_out`).

- **A member extension's override is recorded nowhere, and still overrides.** The frontend's
  override tables carry no edge for `override fun String.decorate()`, so a model reading only those
  tables gives it a slot of its own — and a call through the base's type then reaches the base's
  body, which is a wrong answer with nothing to signal it. The IR answers the question that name
  matching alone cannot: a declaration written WITHOUT `override` is listed as a fresh one, so a
  method absent from that list matching an inherited member by name and machine signature is that
  member's override (`src/native/classes.rs`,
  `tests/native_codegen_e2e.rs::an_override_the_tables_do_not_record_still_dispatches`).

- **An `inner` class carries its outer instance in a field, written first.** `this@Outer` and an
  unqualified read of an outer member are both loads of that field, and one enclosing-instance edge
  is one load, so a doubly nested `inner` class follows one per level. The store runs BEFORE the
  superclass constructor — Kotlin's own order, which a base-class `init` calling an overridden
  method can observe. The JVM needs that order for its verifier; here it is kept because it is the
  language's (`src/native/codegen/lower/objects.rs`,
  `tests/native_codegen_e2e.rs::an_inner_class_reaches_its_enclosing_instance`).

- **A SAM conversion changes which table a function value wears.** A lambda converted to a `fun
  interface` is still an object holding its captures; what differs is that a caller reaches it
  through the interface's own member number rather than a single invoke slot, so its table is as
  long as every class's and starts from the INTERFACE's own — default methods included, and any
  `kotlin.Any` member the interface overrides kept in `Any`'s slot, where the runtime's own
  rendering looks. Converting a NULLABLE function value yields null when it is null, rather than a
  wrapper around nothing (`src/native/codegen/lower/functions.rs`, `src/native/classes.rs`,
  `tests/native_codegen_e2e.rs::a_lambda_becomes_the_fun_interface_it_is_converted_to`,
  `::converting_a_null_function_value_to_a_fun_interface_yields_null`).

- **`is` finds an interface in the type, not on the chain.** Single inheritance gives one superclass
  chain, and an interface is not on it, so each type descriptor carries the interfaces it implements
  — transitively, so an interface's own bases and a superclass's interfaces answer too
  (`src/native/runtime/krusty_rt.c`, `src/native/codegen/lower/objects.rs`,
  `tests/native_codegen_e2e.rs::an_interface_answers_is_and_as`).

- **A data class's members are Kotlin's, down to the per-field hash.** `equals`, `hashCode`,
  `toString` and `componentN` are synthesized by common lowering; what a backend supplies is the
  per-field hash and comparison they are written in terms of, and each has one right answer a
  program can print: `Boolean` hashes to 1231 or 1237, `Long` to `(v xor (v ushr 32)).toInt()`,
  the narrower integers to themselves widened, and a reference through its own `hashCode`. A field
  comparison is `equals`, not the machine's `==`. A field holding a floating-point value is
  declined natively for now, because the `toString` synthesized beside it would have to render one
  (`src/native/codegen/lower.rs`,
  `tests/native_codegen_e2e.rs::a_data_class_gets_kotlins_equality_hashing_and_rendering`).

- **A data class renders an ARRAY field by content, nullable or not.** `A(x=[0, 1], y=null)`. The
  checked lowering has to see through the `?` when it decides a field is an array — `is_array` is
  false for `Array<Int>?` — and the JVM realization has to name `java.util.Arrays.toString(Object[])`
  for a reference array, since no `Integer[]` overload exists to name. Both were wrong, and the two
  wrongs were invisible together: the nullable field never reached the call that would have failed
  (`src/fir_lower/data_classes.rs`, `src/jvm/ir_emit.rs`,
  `tests/dataclass_hash_and_sam_e2e.rs::data_class_array_fields_render_their_contents`).

- **An extension property is its accessors.** `val Cell.doubled get() = value * 2` has no backing
  field — there is no object of its own to keep one in — so every read and write is a call to the
  accessor, with the receiver passed as an argument. The parameter order is Kotlin's declaration
  order: context parameters, then the extension receiver, then the value a setter takes, which is
  what the checked lowering records in `IrFile::local_property_layouts` when it builds the
  accessors (`src/native/codegen/lower/statics.rs`,
  `tests/native_codegen_e2e.rs::an_extension_property_is_read_and_written_through_its_accessors`).

- **A scope function whose block is a function value is still a scope function.** `apply`, `also`,
  `let` and `run` are `inline`, so a block written at the call site is spliced and never reaches a
  backend. A block that arrives as a function-typed parameter (`fun build(instructions: Box.() ->
  Unit) = fresh().apply(instructions)`) has no body to splice, so the call survives and has to be
  realized: invoke the block on the receiver, which is evaluated once, and yield the receiver for
  `apply`/`also` or the block's result for `let`/`run`
  (`src/native/codegen/lower/scope.rs`,
  `tests/native_codegen_e2e.rs::a_scope_function_whose_block_is_a_function_value_runs`).

- **An `inline` call's block is the call site's own code.** `x.apply { … }` and its siblings are
  `inline`, and the checked lowering splices the block into the caller rather than making a
  function value of it. What it leaves behind is a cleared standalone implementation and an
  orphaned lambda node, both unreachable; a backend must emit neither, and must not DECLARE the
  cleared implementation either — an exported symbol that is never defined fails the object's own
  consistency check. Emitting the node's thunk fails later still, at the link, because the thunk
  calls that implementation (`src/native/codegen/lower.rs`, `src/native/codegen/lower/functions.rs`,
  `tests/native_codegen_e2e.rs::the_stdlib_scope_functions_are_expanded_at_the_call_site`).

- **Equality on a function value is declined natively.** Kotlin answers `::f == ::f` with `true`:
  a callable reference compares by the declaration it names and the receiver it binds, not by
  identity. A lambda's `equals`/`hashCode` ARE identity, which the native backend would answer
  correctly — but by the time a value reaches a comparison its type no longer says which it is
  (`val f: (Int) -> Int = ::double` wears the same `Function1` a lambda wears), so the one type
  they share is declined for both rather than answered wrongly for one. Identity through `===`
  stays available, and a capture-free lambda is one object
  (`src/native/codegen/lower.rs`,
  `tests/native_codegen_e2e.rs::comparing_two_function_values_is_declined`).

- **What `tailrec` loops is a FRAME, not a top-level function.** A tail self-call can be stepped
  whenever the next turn reads only slots the step reassigns. That is true of more than a top-level
  `fun` called by name, and the three shapes differ only in where the caller's values sit:

  **An extension** steps its receiver like any other parameter. The receiver is inserted into the
  function's parameter list at its own position and passed as an ordinary argument, so
  `tailrec fun Int.down(acc: Int): Int = if (this == 0) acc else (this - 1).down(acc + 1)` re-binds
  it by assigning that slot. A self-call to a *different* receiver is therefore a loop step rather
  than a shape to decline — the opposite of the member rule below, because here the receiver is part
  of the frame rather than the identity of it.

  **A member** steps when the call dispatches on `this`: the instance does not change, so only the
  parameters are reassigned and the receiver slot is left alone. `Other().f(n - 1)` is a different
  frame, stays an ordinary call, and keeps recursing.

  **The parameters are not always slots `0..n`.** A body's dispatch receiver sits below them, so a
  member's first parameter is one above its `this`; captures and a local class's constructor values
  take slots before that. The lowering is the authority (`BodyLowering::value_slot`) and reports the
  layout it used, rather than the rewrite deriving it a second time and being wrong when the two
  disagree.

  **A member call does not read `this` at the call node.** The lowering spills the receiver and each
  argument into generated temporaries first — evaluation order being the point — so the shape
  reaching the rewrite is `{ t0 = this; t1 = n - 1; this.f(t0, t1) }` with the call reading `t0`. The
  rewrite follows those aliases to a fixed point through unnamed temporaries only, and drops any slot
  the body reassigns. A source `val` is never followed: what stays in it is the programmer's
  business. Being conservative costs only a missed rewrite, and a missed rewrite is the program that
  was compiled before.

  **A LOCAL function's frame is a physical capture prefix and then its logical parameters.** The
  rewrite is driven from declaration lowering, and a local function reaches the IR by another path
  that never ran it, so every `tailrec fun` inside a function kept its self-call and overflowed at
  the depth the modifier exists to make safe. Lifting gives a local its captures as LEADING
  parameters, and every call to it — the recursive one included — passes them; `BodySlots::
  first_parameter` already points past them. Everything after that point is a logical parameter in
  declaration order: the context parameters, an extension receiver where there is one, then the
  declared value parameters. The loop reassigns exactly those and leaves the capture slots alone,
  which is sound because a self-call's capture arguments re-read the frame's own capture
  parameters — checked, not assumed, so a call whose prefix is anything else stays an ordinary
  call.

  Counting either end of the list alone is what left whole source forms recursive, each a normal
  Kotlin program that kotlinc runs flat: taking the IR list's length writes a capture slot, and
  taking the declaration's parameters minus its context values makes the self-call test compare the
  call's whole argument list against a smaller number, so a CONTEXTUAL local declined in silence —
  and a local EXTENSION, whose receiver the IR carries as an ordinary parameter at its own
  position, was miscounted the same way. A capture is an implementation detail of lifting, not a
  Kotlin reason to revoke the constant-stack contract. A physical list SHORTER than that prefix is
  an invalid checked shape rather than a frame with no logical parameters, so it fails closed
  (`FirLoweringFailure::MalformedLocalFrame`): saturating there would hand the loop a frame that
  reassigns nothing, which is the same silent decline in a different disguise.

  **A local declared inside a class MEMBER is lifted onto that class**, as a private static, so its
  self-call is a `Callee::ClassStatic` rather than a `Callee::Local` — the same declaration reached
  through the owner it was lifted onto. The self-call test recognized only `Local`, so this shape,
  which is ordinary Kotlin and which kotlinc runs flat, recursed until `StackOverflowError`. The
  callee IDENTITY answers it; the owner's spelling is not consulted. Test:
  `a_class_member_local_tailrec_runs_flat` — a plain member's local, one that captures a property,
  and one in a companion, each a million deep and each compared against the reference compiler.
  Tests: `tests/tailrec_e2e.rs` (`a_member_tailrec_runs_flat`, `an_extension_tailrec_runs_flat`,
  `a_member_call_on_another_instance_still_recurses`, `a_local_tailrec_runs_flat`,
  `a_class_member_local_tailrec_runs_flat`,
  `a_capturing_local_tailrec_runs_flat` — read-only, mutated, and both at once —
  `a_contextual_local_tailrec_runs_flat`, `an_extension_local_tailrec_runs_flat`, each a million
  deep and each asking the reference compiler the same question, and
  `member_and_extension_tailrec_agree_with_kotlinc` — a `StackOverflowError` on one side and an
  answer on the other is the divergence they report).

- **A `return` is a tail position wherever it stands.** `tailrec` rewrites a tail self-call into a
  loop step, and the tail positions of a function are not only its last expression: nothing of the
  function runs after a `return`, so `if (n > 0) return f(n - 1)` written before the body's final
  statement is a tail call too. The rewrite used to reach only the body's last root, so such a
  function was left recursing — and a `tailrec` left recursing is not a slower answer but a
  `StackOverflowError`, which is exactly the outcome the modifier was written to rule out.

  **What bounds the search is OWNERSHIP, not syntax.** A loop is not a boundary: Kotlin reads
  `while (…) { … return f(x) }` as a tail call, and the `continue` the rewrite writes carries the
  synthetic loop's own label, so leaving the inner loop is the rewrite working rather than a reason
  to skip it. Two things do stop it. An inlined LAMBDA's body owns its own `return`s — a depth-zero
  one there is the lambda's, which is the one shape the checked depth cannot tell apart from this
  function's, so the boundary rather than the depth is what keeps the walk out; its captures are
  ordinary expressions of the enclosing function and stay in the walk. A `try` has a `finally` that
  still has to run, so a `return` inside it does not leave directly — the ordering of that `finally`
  is what a test can see, and does.

  **Whether a `return` is THIS function's is the checked depth, not the shape it was found in.**
  `IrFile::checked_return_depths` is the authority: depth zero is a return of this callable and a
  deeper one targets a frame outside it; a `return` lowering generated itself carries no depth and
  is this function's by construction. A slot that stops being a `Return` gives up its entry with it,
  because the side table's contract is that only a return node carries one.

  **The rewrite happens in place, and one root-to-node path is the whole licence for that.** Common
  IR is a DAG: lowering may hand one ancestor id to two parents, so its descendants are observed on
  both paths even when each has one direct parent node. Every structural edge is inventoried first —
  including the ones the rewrite will not follow — and multiple reachability is propagated through
  descendants. A `return` reached by more than one path is left alone. That program keeps recursing,
  which is the answer this pass started from and is never a wrong one.

  A trailing LOOP is left as the statement it is rather than wrapped in a `return`: a loop is not a
  value and has no tail position of its own, and returning one is what a body ending in
  `while (true) { … return f(x) }` used to compile to, which the verifier rejected.
  Tests: `tests/tailrec_e2e.rs` (`tailrec_through_a_return_that_is_not_the_last_statement`,
  `tailrec_through_a_return_inside_a_loop`, `a_return_a_finally_still_follows_is_not_rewritten`,
  `a_lambda_in_the_body_keeps_its_own_returns_while_the_body_is_rewritten`,
  `a_call_that_only_looks_like_a_tail_call_still_recurses`, and
  `tailrec_rewriting_agrees_with_kotlinc`, which asks the reference compiler the same questions);
  `src/fir_lower/tailrec.rs` (`a_return_the_dag_shares_with_a_try_is_left_alone`,
  `a_return_below_a_shared_ancestor_is_left_alone`,
  `a_return_that_targets_an_outer_frame_is_left_alone`,
  `a_lambdas_capture_is_swept_and_its_inline_body_is_not`,
  `a_return_reached_by_one_path_becomes_a_loop_step`), which state the three rules above on the IR
  directly — a DAG edge crossing an opaque boundary has no Kotlin source that produces it.

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
  type (written in terms of the property's own parameters). Tests:
  `tests/toplevel_property_ref_e2e.rs::toplevel_property_refs_run`,
  box `callableReference/property/extensionPropertyWithExtensionType.kt`.

- **A property reference names the accessor the declaration actually realizes, value classes
  included.** krusty emitted a reference class that called an accessor declared nowhere, so the
  program failed at its first `get` with a `NoSuchMethodError` — an unlinkable artifact emitted
  without a diagnostic. Four rules, each measured against the reference compiler's own emitted body
  (box `inlineClasses/callableReferences/*`, 14 cases; box total 6317 → 6337, twenty cases moving
  and none regressing — the `annotations/instances/annotationInstances*` pair flips run to run on
  any commit and is excluded):
  - A **member** of a value class is realized statically over the erased carrier, so the reference
    casts its receiver, unboxes it, and calls statically: `checkcast Z; Z.unbox-impl()I;
    Z.getXx-impl(I)I`. The `-impl` suffix is the structural form the declaration side uses when the
    signature needs no value-class hash of its own.
  - An **extension on** a value class is the same shape through its facade, with the hash-mangled
    name the declaration carries: `ExtKt.getXx-IQRRRT4(I)I`, not `getXx(LZ;)I`.
  - Which of those two a reference is, is RECORDED, never inferred. `PropRef::ext_facade` is `Some`
    for an extension AND for a private member reached through an `access$…` bridge — the bridge's
    accessor is static in the same way, so it names an owner there rather than a facade. Reading it
    as "this is an extension" rebuilt `getXx-IQRRRT4(I)I` for a private member whose declaration is
    `getXx-impl(I)I`, and `(this::xx).get()` on a value class failed with
    `NoSuchMethodError: 'int Z.getXx-IQRRRT4(int)'`. `PropertyReferenceRealization::accessor_role`
    now carries the selection made where the three cases are distinguishable, and a private member
    of a value class
    takes the member rule with kotlinc's bridge in front of it: `Z` declares
    `private static final getXx-impl(I)I` and publishes `public static final access$getXx-impl(I)I`
    beside it, and the reference calls the bridge. Publishing the accessor ITSELF was a declaration
    bug one layer below references, and not value-class-specific: `materialize_member_property` —
    the path a member property with a CUSTOM accessor takes — recorded its accessors' source order
    but never entered them in `ir.private_methods`, unlike the two sibling paths that handle
    backing-field properties, so every private computed member property was emitted `ACC_PUBLIC`. A
    value class made it visible because there the accessor is also renamed and called from a
    separate class. The bridge is the existing function-reference access-bridge emitter, which had
    no populating caller and assumed an instance target; a value class's accessor is already
    callable without an instance, so it takes a STATIC target too — the bridge carries exactly its
    parameters and `invokestatic`s it.
  - The value class's own **underlying** property is the exception and takes no rewrite: reading it
    is the unbox, and its accessor stays an ordinary instance getter on the box (`Z.getX()I` —
    which is also the signature the reference reports, exactly as kotlinc's does, even though
    kotlinc's body shortcuts to `unbox-impl`). WHICH property that is, is the declaration's own
    storage position — a value class has exactly one field, and the property that owns it is the
    underlying one — or, for a dependency, the underlying property its `@Metadata` names. It is not
    a spelling: `x` names the underlying property of `Z`, of `S`, and of any unrelated class that
    happens to declare one, so comparing the reference's property name against a list of underlying
    names answers for the wrong declaration as readily as the right one.
  - A property whose **TYPE** is a value class already carried the mangled accessor name, but a
    member or top-level property has no written descriptor, so the one synthesized from its
    semantic type (`()LZ;`) named a method the declaration does not have: it exchanges the carrier
    (`()I`, `(I)V`). The descriptor is now synthesized from the carrier, which also gives
    `box-impl`/`unbox-impl` at the `KProperty` boundary their correct signatures.

  A **top-level** property of value-class type is realized over the CARRIER as well, so the rule
  above has no exception left to carve out. kotlinc emits `private static int topLevel`,
  `public static final int getTopLevel()` and `public static final void setTopLevel-IQRRRT4(int)`,
  and krusty emits that surface now. The setter's name mangles and the getter's does not, which is
  `vc_mangle`'s standing rule read at a file facade: a value-class PARAMETER always contributes to
  the hash, a value-class RESULT contributes only outside a file class (`is_file_class` suppresses
  the return contribution — a member keeps its `getZ-a_XrcN0()I`, the facade's `getTopLevel()I` has
  no hash at all). Both halves of that realization are RECORDED on the declaration rather than
  recomputed at each site that has to agree with it: `IrStatic::erased_value_class` names the class
  the storage was erased over (`None` ⇒ the boxed convention) and `IrStatic::setter_jvm_name` the
  setter's spelling (`None` ⇒ the ordinary `set<X>`), and the set of erased facade properties is
  handed to the reference pass. A reader that re-decides is a reader that can disagree: while the
  declared type still said `LZ;` and the field said `I`, the value-class boxing analysis read
  `getstatic gz:I` as a BOXED value and a defaulted parameter `fun test(z: Z = gz)` got
  `Integer.valueOf; checkcast Z; unbox-impl` over a carrier that was never boxed — a
  `ClassCastException` at the first call. The decision is therefore taken once, before any boxing
  analysis runs, and the storage erased afterwards, where the initializer rewrites that still speak
  in the declared type are already done. A COMPANION property's
  field stays BOXED (`LX;`) with its initializer boxed to match: only the top-level one lives on
  the facade kotlinc erases, and holding both under one rule is what named a `getTopLevel()LZ;` no
  declaration had. The reference's reflection owner and top-level flag keep reading `ext_facade`
  alone: a member of a value class is still a MEMBER reference (`ldc Z.class`, flags 0), and only
  the physical call shape changes. A RECEIVERLESS accessor's descriptor stays the property's own
  type rather than the `PropRef`'s recorded one, which for a companion-block or access-bridged
  property names the owner it is called with — a descriptor this reference passes nothing for (it
  emitted an `invokestatic` on an empty stack). Tests:
  Every one of these answers is a JVM realization fact, so it is recorded where the selection is
  made — in `jvm::property_references::PropertyReferenceRealization`, keyed by the synthesized
  reference class's own internal name — and not on the common-IR `PropRef`, which says which
  property a reference names and what its `KProperty` surface is. The record carries the accessor
  names the declaration wrote, the realization shape, whether the storage is a facade's own,
  whether the property IS its value class's storage, the PHYSICAL return the selected getter
  declares, and the exact module functions the accessors are. The last two are what a later pass
  would otherwise rebuild from a rendering: reading a return back out of a descriptor this compiler
  itself synthesized makes a spelling the authority over a declaration, and an `access$…` bridge
  found by rebuilding a mangled name and looking for a method that answers to it is a bridge to
  whatever happens to be spelled that way. The bridge is now named after the function the selection
  recorded, and a reference with no recorded accessor refuses the file rather than naming something
  nothing declares. Tests:
  `tests/value_class_property_reference_e2e.rs` — the member, private-member, underlying and
  top-level cases each compare krusty's COMPLETE emitted class list and method surface against the
  reference compiler's, and name each reference class rather than searching the dump for one whose
  body looks right — and
  `companion_e2e::companion_block_mutable_property_reference_is_receiverless`.
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

  A local class's BODY properties are numbered from the body, and the stable declaration of each is
  read from the active-declaration table under that same coordinate
  (`ActiveSourceDeclarations::class_body_property_declaration`, the class's parser id and the body
  index) — the binding itself, never anything reconstructed from another numbering. Reconstruction
  is where the defect was: the legacy `SourceMember` coordinate numbers a class's constructor `val`
  parameters first and the body's properties after them, so the two agree only when there are no
  constructor `val`s, and reading one as the other names a LATER property. In
  `class P(val n: Int) { val a = n * 2; val b = a + 1 }` every reference to `a` bound to `b`, so
  `b` read an unwritten field and `p.a` answered with `b`. The regression runs both kotlinc-built
  and krusty-built JVM classes and asserts their exact results (`tests/local_class_e2e.rs`).

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

- **A flow narrowing does not survive a loop that writes its subject.** A straight-line proof is a
  proof about ONE edge, and a loop has a back edge: a body that reassigns `x` reaches its own start
  with whatever that assignment left. So the writes a loop performs anywhere inside it — including
  in nested lambdas and local functions, which run on that same edge — clear the narrowing before
  the loop is checked at all. Four of kotlinc's answers follow from doing it there rather than on
  the body scope: the CONDITION sees it (`while (x.length > 0) { x = 42 }` is rejected), the body
  sees it, the code after the loop sees it, and a `do…while` sees it even though its first
  iteration precedes any back edge. What must NOT be cleared is the narrowing the loop's own
  condition proves — that one is re-evaluated on every iteration, so `while (x != null) { x.length;
  x = null }` stays legal — and it survives because condition narrowings are computed after this
  clearing and applied to the body scope. Narrowing within one iteration is unaffected: the
  loop's own assignment proves the new type from that point on.
  krusty used to keep the stale proof and emit a cast from it, which failed at run time on both
  backends (`codegen/box/casts/kt83324.kt`, `codegen/box/objectExpression/expr3.kt`).
  Tests: `tests/loop_backedge_narrowing_e2e.rs` (all eight, each answer taken from kotlinc 2.4.10
  first).

- **A value class delegating an interface uses its own UNDERLYING VALUE, not a field of the
  delegation's.** A value class has exactly one field, and `value class IC(val i: I) : I by i` names
  that very field as the delegate. Kotlin synthesizes no `$$delegate_0` there — the underlying value
  IS the delegate, and every forwarder reads it — and it could not: a second field is not a shape a
  value class has.

  Common lowering synthesized one anyway. That gave the class two fields, which the JVM emitter
  turned into a `putfield` of the wrong type in `constructor-impl` — the class was rejected at load
  with `VerifyError: Bad type on operand stack in putfield` — and which the native code generator
  refused by name.

  WHICH field the delegate is cannot be decided where the delegation field used to be created: the
  property's own field does not exist yet at that point, and the delegate field was being pushed
  ahead of it. So nothing is pushed for this shape and the edge is recorded later, where the
  constructor's field indices are known. The delegate must be the first CONSTRUCTOR PARAMETER, which
  is what the underlying value is; a value class delegating to anything else is not this shape and
  keeps the ordinary field, to be refused as before rather than silently pointed at the wrong
  storage.
  Tests: `tests/value_class_delegation_e2e.rs`, each cross-checked against the reference compiler:
  the forwarder reached through both types, the underlying value still readable as its own property,
  a generic underlying type, and an ORDINARY class still delegating through a field of its own.
  Corpus: `codegen/box/inlineClasses/delegationByUnderlyingType/` (all six).

- **A local class whose SUPERCLASS is a local class with captures passes them on.** A capturing
  local class takes its captures as synthetic PREFIX parameters of its constructor, ahead of the
  ones the source wrote. A subclass's `super(…)` spells only the written ones — the prefix is not in
  the source and there is no expression there for a resolved-constructor lookup to find — so the
  call was one value short per capture. kotlinc compiles these; krusty rejected them on both
  backends, the JVM's with `VerifyError: Bad type on operand stack` putting `this` where the capture
  belonged.

  Two halves, neither sufficient alone:

  - Selection records the superclass's captures as the subclass's own, read from the RESOLVED
    SUPERTYPE (`resolved_body_local_supertypes`) rather than from a call. That is the one edge a
    supertype constructor gives: `class Derived : Local(true)` records the base classifier and its
    arguments and nothing in between.
  - Common lowering prepends the matching prefix reads to `super_args`. Each transitive capture
    retains the superclass field's stable semantic coordinate, so matching never depends on a
    synthetic field spelling. Only a call short by exactly the parent's prefix is filled; any other
    shape is left to the arity check downstream.

  The same lexical value captured twice is ONE capture. A class that captures `x` for its own body
  and is then found to need `x` for a declaration it reaches carries one field, not two — the second
  is a duplicate field of the same name, which the class file format rejects outright
  (`ClassFormatError: Duplicate field name`). The merge previously keyed a dependency-required
  capture on the dependency alone, so an identical own capture did not match it.

  Anonymous objects use the same resolved-superclass edge after their body-driven capture pass, so
  they also carry a superclass capture that their own body never mentions.
  Tests: `tests/local_superclass_capture_e2e.rs`, seven shapes, each cross-checked against the
  reference compiler. Corpus: `codegen/box/localClass/localHierarchy.kt`,
  `codegen/box/innerNested/superConstructorCall/{localExtendsLocalWithClosure,localWithClosureExtendsLocalWithClosure}.kt`,
  `codegen/box/localClasses/innerOfLocalCaptureExtensionReceiver.kt` and
  `codegen/box/secondaryConstructors/callFromLocalSubClass.kt`.

### Smart casts

- **A narrowing recorded for a bare member access belongs to the implicit `this` inside it, not to
  the value the access produces.** `resolve` records `narrowed_this_member` against the *access*
  (`node` for an implicit `this.node`) so the lowerer narrows the `this` it loads before reading the
  field. When such an access is itself a call's receiver — `node.tag()` — the checked call path must
  not read that map again: the access has already applied the narrowing, and casting a second time
  checks the PROPERTY's value against the receiver's narrowed type. `node.tag()` inside
  `if (this is Light)` emitted `checkcast Light` twice, the second on the `Node` that `getNode`
  answered, which the JVM verifier rejects outright (`Type 'Light' is not assignable to 'Node'`) and
  the native backend turns into a `ClassCastException`. Only `selected_value_smartcasts` — a cast of
  the receiver's OWN value — belongs at a call's receiver.
  Tests: `tests/narrowed_this_member_call_e2e.rs`
  (`a_member_read_on_narrowed_this_may_be_a_calls_receiver`,
  `a_narrowed_read_carries_its_siblings_as_arguments`); the corpus case is
  `codegen/box/smartCasts/kt44814.kt`.

### Native target (`src/native/`)

The native backend has no `kotlinc` to be differential against — Kotlin/Native's output is LLVM
bitcode, not comparable bytes — so each decision below is recorded here with the test that pins it,
and behavior is checked by RUNNING the emitted program.

- **A walk answers `first`/`last`/`isEmpty`/`lastIndexOf` only for a receiver the runtime cannot
  index.** The walk table is consulted before the list table, so an entry added to it shadows the
  list's. For most shared names that is harmless — `indexOf` and `contains` ask the elements the
  same question either way — but `first`/`last` are not: Kotlin's list form raises
  `NoSuchElementException("List is empty.")` where the walk form says `"Collection is empty."`. The
  call site therefore holds those four names back for a list whose elements the runtime owns, and
  hands them to the walk only where the file puts a class of its own behind the collection type, in
  which case the list path declines outright and the walk is the only answer there is.
  Tests: `tests/native_iterable_walk_members_e2e.rs`
  (`an_empty_walk_raises_with_the_collection_wording`,
  `an_empty_list_still_raises_with_the_list_wording`).

- **`find` is `firstOrNull` with a predicate.** Kotlin declares it as an alias with the same body,
  so both names reach one runtime function rather than two alike.
  Tests: `tests/native_iterable_walk_members_e2e.rs`
  (`a_walk_finds_the_first_element_matching_a_predicate`).

- **`c in s` and `t in s` are different entry points, not one with a converted operand.** The first
  asks about a single UTF-16 unit and the second about one text inside another, and the two answers
  differ whenever the char is one Kotlin encodes in several bytes. Both go through the
  `ignoreCase` mechanism — the flag's default arrives as a materialized constant from a klib and as
  an absent argument from a jar, and anything but a literal `false` still declines, case folding
  being a question about Unicode the runtime holds no table for — so the selected entry point now
  carries its OPERAND's type with it: a machine `Char` for one, a reference for the other.
  Tests: `tests/native_text_members_e2e.rs` (`a_char_is_looked_for_inside_text`).

- **A text entry point that SHARES the receiver's storage must be given a string.**
  `kt_string_substring` reads a string's own storage fields directly so the result can name the
  same bytes, but every text member here is declared over `CharSequence`, and a `StringBuilder` is
  one with a different layout — the fields read would be whatever a builder holds at those offsets,
  which in practice reads as a length of zero. `kt_string_length` and `kt_to_string` are the two
  that already answer for both, so the receiver is converted before the fields are read. A jar
  provider resolves `sb.substring(…)` to `java.lang.StringBuilder.substring` and it declines by
  name; a klib provider has no such class, and the same call lands in the `kotlin.text` facade.
  Tests: `tests/native_text_members_e2e.rs` (`a_builders_own_substring_is_declined_by_name`,
  `a_builder_replaces_one_of_its_units`).

- **`String.toInt()` accepts an optional sign and ASCII digits, and nothing else.** No whitespace,
  no radix prefix, no trailing text; a magnitude past the type's bound is the same
  `NumberFormatException` as a text that is not a number at all, as on the JVM. The bound is not
  symmetric — `Int.MIN_VALUE` has no positive counterpart — so the sign is read before the limit is
  applied, and the accumulator is checked BEFORE each multiply so a long run of digits is refused
  rather than wrapping.
  Tests: `tests/native_text_members_e2e.rs` (`text_parses_itself_as_an_integer`,
  `text_parses_itself_as_an_integer_or_nothing`).

- **`isNullOrBlank`/`isNullOrEmpty` take the null.** They are the only text members Kotlin declares
  on a nullable receiver, and that is their whole point, so the null reaches the runtime instead of
  being checked away at the call site.
  Tests: `tests/native_text_members_e2e.rs` (`text_answers_whether_it_is_null_or_blank`).

- **`single()` has two different complaints.** An empty text raises `NoSuchElementException` and a
  longer one `IllegalArgumentException`: "there is none" and "there is more than one" are different
  mistakes, and Kotlin reports them as such.
  Tests: `tests/native_text_members_e2e.rs` (`text_answers_its_single_char`).

- **A `Unit` RETURN is nothing; a `Unit` PARAMETER is the singleton.** A function that answers
  `Unit` answers the one value there is, and a caller that needs it can name it without being
  handed it — so the return carries nothing, which is what keeps a `Unit`-returning call free. A
  parameter cannot do the same: the program can compare it by identity (`y !== Unit`), pass it on,
  store it as `Any`, and declare extensions whose receiver it is (`fun Unit.foo()`), so it carries
  the reference to the runtime's singleton exactly as the JVM passes `kotlin.Unit.INSTANCE`. The
  asymmetry is why `parameter_carrier` is separate from `carrier` rather than a change to it. A
  `Unit`-typed LOCAL is a third case and keeps its own treatment: its declaration stores nothing
  and a read of it materializes the singleton.
  Tests: `tests/native_unit_value_e2e.rs`; the corpus cases are `codegen/box/basics/unit4.kt`,
  `codegen/box/extensionFunctions/extensionFunctionDifferentReceivers.kt` and
  `codegen/box/extensionProperties/extensionPropertyDifferentReceiver.kt`. krusty's JVM backend
  does not lower these shapes yet — it emits a body the verifier rejects with
  `Operand stack underflow` at the store after a `Unit`-valued expression — so those tests take
  kotlinc as the oracle and require only the native backend to match.

- **An array member's operands are carried as its TABLE says, not all as references.** Every entry
  on that path took references, which is right for all of them but one: `copyOf`'s size is an
  `Int`, and boxing it to hand it over is not a slow path but a signature Cranelift's verifier
  rejects outright. The call site now coerces each operand to the carried type the entry names.
  Tests: `tests/native_array_members_e2e.rs` (`an_array_copies_itself_to_a_length`).

- **`contentDeepToString` renders an array that contains itself as `[...]`.** At any nesting depth,
  and without looping: the arrays currently being rendered are a chain of frames on the C stack,
  bounded by the nesting rather than by the element count. This is Java's `deepToString` rule and
  Kotlin's documented one.
  Tests: `tests/native_array_members_e2e.rs` (`an_array_renders_its_contents_deeply`).

- **`super<B>.p` names B's REALIZATION of the property, not a declaration on B.** An `open val` on
  an interface B merely inherits is what `super<B>.p` reads, and `super<C2>.p` reads the `p` that
  C2's own superclass declares. The search therefore walks up from the named class — its superclass
  chain and the interfaces met on the way, breadth-first, which is the order a realization is
  inherited in — and answers the first declaration it meets. Searching the named class alone found
  nothing and declined. A PROPERTY is what makes this show: `super.f()` on a method finds a method
  to call, but a class whose accessors are the default ones declares no method for its property.
  Tests: `tests/native_super_property_e2e.rs`; the corpus cases are
  `codegen/box/super/kt3492TraitProperty.kt`, `codegen/box/basics/superSetterCall.kt`,
  `codegen/box/super/{kt14243_prop,unqualifiedSuperWithDeeperHierarchies}.kt` and
  `codegen/box/traits/kt5393_property.kt`.

- **A SPECIAL BRIDGE stands in front of a collection member a file implements itself.**
  `Collection<E>.contains(x as E)` erases to a call taking anything and `EmptyMap.get(null)`
  reaches a `get(key: Any)`; Kotlin answers such a call WITHOUT running the override, testing the
  argument against what the override declares and handing back the member's own default when it is
  not one. These members used to decline outright for a file with its own collection, because the
  dispatch among that file's classes had no bridge to put in front of each arm.

  The default is read off the member's ANSWER — `false` for the questions, `-1` for the positions,
  `null` for the lookups — rather than named per member, because the two have to agree:
  `Map.remove` answers the value it removed while `MutableCollection.remove` answers a boolean, and
  a per-name table got exactly that pair wrong, emitting an `i8` into a merge block expecting a
  pointer.

  Which override a call can reach is decided by DIRECTION. Where the call states a reference — the
  erased position the bridge exists for — any override is reachable, a scalar one
  (`containsValue(value: Int)` on a `Map<String, Int>`) by testing the box's descriptor and
  unboxing. Where the call states a scalar there is nothing to widen from, so only the same scalar
  can be meant: without that, `removeAt` — which a JVM realization spells `remove(int)` — bound to
  a class's own `remove(String)` and answered `null` for `list.removeAt(0)`.

  The implementors are found by SHAPE, because that is how the caller decided there was something
  of this file's behind the receiver: `StrList : List<String?>` names `List` and never
  `Collection`, and a receiver typed `Collection` is what the decline was about.
  Tests: `tests/native_special_bridge_e2e.rs`. krusty's JVM backend does not emit the bridge
  METHODS, so the two cases that need one there take kotlinc as the oracle and require only the
  native backend; that gap is unchanged by this and confirmed by stripping every `src/native/`
  change from the tree.

- **A map's three lookups disagree about a stored NULL, on purpose.** `getValue` is written in
  terms of "absent" and raises only then, so a key whose value is null answers that null.
  `getOrElse` and `getOrPut` are written in terms of "null" and run their lambda for a stored null
  as readily as for a missing key. Each follows its own declaration rather than being made to
  agree with the others, because the difference is Kotlin's and a program can see it. Neither
  lambda is told the key — unlike a LIST's `getOrElse`, which is handed the index.
  Tests: `tests/native_map_members_e2e.rs` (`a_map_answers_a_key_it_holds_or_raises`,
  `a_map_falls_back_and_optionally_keeps_the_fallback`).

- **`val x by map` reads the map under the property's own NAME, through `getValue`.** So a missing
  key raises rather than answering null: a delegated property whose type is not nullable has no
  null to answer with. The name is a literal taken from the reference's own declaration — the
  runtime cannot ask a `KProperty` object for it — which is the same rule the read-write delegates
  already follow.
  Tests: `tests/native_map_members_e2e.rs` (`a_property_delegates_to_a_map_under_its_own_name`).

- **`for (x in someIterator)` walks the very object it was given.** Kotlin declares
  `Iterator<T>.iterator()` answering `this`, so the walk shares the receiver's position: a second
  walk of the same iterator finds only what the first left.
  Tests: `tests/native_map_members_e2e.rs` (`an_iterator_is_its_own_iterable`).

- **A receiver typed `Collection` proves nothing about its LAYOUT.** `Collection` is admitted as a
  list type, because the list entry points answer every question a list-shaped collection is asked.
  But a `Set` is a collection too and is not shaped like a list, so `kt_list_size` read its count
  out of fields the object does not have — a garbage number, silently, which is the one answer a
  backend must never give. `setOf(1, 2, 3).size` was right; the same set behind a `Collection<*>`
  was not. The runtime asks the OBJECT and counts the walk for anything that is not one of its two
  list shapes; a real list never reaches that path. The same reasoning as the text members' string
  receiver: a static type that names a SUPERTYPE cannot be read as a promise about storage.
  Tests: `tests/native_iterable_walk_members_e2e.rs`
  (`a_set_behind_a_collection_still_counts_itself`); the corpus case is
  `codegen/box/collectionLiterals/stdlibCollections.kt`.

- **`zip` stops at the shorter walk and asks the second only while the first has more.** Kotlin's
  own loop is `while (first.hasNext() && second.hasNext())`, and the short-circuit is observable
  through an iterator with a side effect.
  Tests: `tests/native_iterable_walk_members_e2e.rs`
  (`a_walk_zips_with_another_and_collects_into_a_set`).

- **`getOrElse` hands the INDEX to its lambda.** Not the list and not nothing — the fallback is
  computed from the index that was out of range.
  Tests: `tests/native_iterable_walk_members_e2e.rs`
  (`a_list_falls_back_for_an_index_it_does_not_hold`).

- **A sort is STABLE, and `sortedBy` asks its selector once per COMPARISON.** The runtime sorts by
  insertion, so equal elements keep the order the walk gave them, which is what Kotlin promises.
  Kotlin's own `sortedBy` is `sortedWith(compareBy(selector))`, which calls the selector inside the
  comparator rather than once per element — a difference a selector with a side effect can see, so
  the runtime does the same.
  Tests: `tests/native_iterable_walk_members_e2e.rs`
  (`a_walk_sorts_by_its_elements_or_by_a_selector`).

- **`minOrNull`/`maxOrNull` answer the FIRST of several equal winners.** The comparison a candidate
  must win is strict, so a later element that merely ties does not displace the incumbent — which
  is how Kotlin's are written and is observable whenever the elements are distinguishable beyond
  what the ordering compares (`minByOrNull` on equal keys).
  Tests: `tests/native_iterable_walk_members_e2e.rs`
  (`a_walk_answers_its_smallest_and_largest_element`).

- **`sort()` is the LIST's, `sorted()` is the walk's.** `sort` reorders the receiver and answers
  nothing; `sorted` leaves it alone and answers a new read-only list. They are different members,
  not two spellings.
  Tests: `tests/native_iterable_walk_members_e2e.rs` (`a_mutable_list_sorts_itself_in_place`).

- **`isEmpty`/`isNotEmpty` are asked of a `Collection`, not of an `Iterable`.** Kotlin declares
  neither over `Iterable` — `isEmpty` is a member of `Collection` and `isNotEmpty` an extension of
  it — so a receiver typed `Iterable` does not have them to be asked at all.
  Tests: `tests/native_iterable_walk_members_e2e.rs` (`a_collection_answers_whether_it_is_empty`).

- **`Int?` is a reference, `Int` is a machine scalar.** A nullable primitive has to represent
  `null`, so it boxes, exactly as it does on the JVM. Anything else would need a sentinel value,
  and Kotlin has no integer that is not a legal `Int`.
  Tests: `src/native/codegen/lower.rs` (`a_nullable_primitive_is_carried_as_a_reference`).

- **`compareTo` on floating-point values is a TOTAL order, not C's `<`/`>`.** Kotlin orders every
  `NaN` above every other value (including itself) and `-0.0` below `0.0`; C's comparison operators
  answer `false` to all four relations on `NaN`. The runtime falls back to the IEEE-754 bit patterns
  after the ordinary comparisons, which is what `java.lang.Double.compare` does and what Kotlin's
  `Double.compareTo` is specified to do. The same operation reached through `<` (`a < b`) keeps IEEE
  semantics — the two spellings genuinely differ in Kotlin.
  Tests: `tests/native_codegen_e2e.rs` (`comparisons_and_boolean_logic_run`); the ordering itself
  lives in `src/native/runtime/krusty_rt.c`.

- **`compareTo` is a runtime call, not an emitted `a < b ? -1 : …`.** The lowered form has the
  receiver and the argument as arbitrary expressions, and a ternary would evaluate each of them
  twice — changing the program whenever either has a side effect.

- **A floating-point value cannot be rendered, and therefore cannot be boxed.** Kotlin's
  `Double.toString` is Java's shortest-round-trip algorithm: `1.0` prints as `1.0`, `1e20` as
  `1.0E20`. C's `%g` is not that algorithm — it printed `1` and `1e+20` — so the runtime was already
  producing strings Kotlin never would. The runtime therefore has no `kt_box_double`/`kt_box_float`
  at all, which makes it *impossible* for a floating-point value to reach a position where something
  would render it: `println(1.0)` is declined at compile time with a diagnostic instead. Arithmetic
  and `compareTo` on floating-point values are unaffected.

- **Concurrency follows Kotlin/Native's memory model, not the JVM's.** The native target is a
  Kotlin Multiplatform target with no Java interop, so its contract is Kotlin/Native's: `@Volatile`
  (`kotlin.concurrent.Volatile`) makes backing-field reads and writes atomic and writes visible to
  other threads — and *only* backing-field operations, so a property whose accessor touches the
  field more than once is not atomic as a whole — and `kotlin.concurrent.atomics` supplies
  compare-and-swap. Two JVM rules are deliberately NOT reproduced because Kotlin/Native does not
  have them: `synchronized` does not exist on this target, and there is no `final`-field freeze, so
  no release fence is emitted at constructor exit. Requiring the JVM memory model would be a
  stronger guarantee than Kotlin/Native offers, so code correct under Kotlin/Native stays correct
  here. Nothing concurrent is implemented yet; this records the target the implementation aims at.

- **The native runtime is freestanding — it uses no C library.** This is what makes
  cross-compilation work: `clang` compiles for every architecture and `ld.lld` links all of them,
  but a libc call would demand a target sysroot (headers plus a target C library) for each one,
  which is the per-architecture toolchain problem the native track exists to avoid. The runtime
  issues `write`, `mmap` and `exit_group` directly through a syscall shim per architecture, uses
  only the freestanding headers C11 §4 guarantees (`stdint.h`, `stddef.h`, `stdbool.h`), defines
  `memcpy`/`memset` itself (a compiler may synthesize calls to them), and supplies its own `_start`
  — in assembly, because at process entry the stack is aligned as if nothing had been called, which
  is not the alignment a compiled function's prologue assumes.
  Tests: `tests/native_codegen_e2e.rs`
  (`one_host_links_a_static_executable_for_every_supported_architecture`), which asserts each
  produced binary's ELF machine number rather than trusting that the cross build happened.

- **`String` is UTF-8 bytes in the native runtime, and exposes no `length`.** Kotlin's
  `String.length` counts UTF-16 code units, which is not the byte count for any non-ASCII text.
  Exposing a byte count under that name would be wrong for `"é".length`, so the runtime exposes
  nothing rather than something wrong. A string constant containing an unpaired surrogate — legal
  in Kotlin, unencodable in UTF-8 — makes the backend decline the file with a diagnostic.
  Tests: `tests/native_codegen_e2e.rs` (`a_string_literal_with_an_unpaired_surrogate_is_declined`,
  `non_ascii_text_survives_the_round_trip`).

- **`+`, `-`, `*` and unary `-` wrap; `/` and `%` go through the runtime; shifts mask their count.**
  Kotlin wraps integer arithmetic on overflow, and so do the machine's `iadd`/`isub`/`imul`/`ineg`,
  so the code generator emits them directly. Division is different: the machine traps on both a zero
  divisor and `Int.MIN_VALUE / -1`, where Kotlin throws for the first and wraps for the second, so
  `/` and `%` are calls to `kt_div_*`/`kt_rem_*` (the runtime aborts with a message on zero, having no
  exceptions yet). A shift count outside `0..31` (or `0..63`) is masked in Kotlin; Cranelift's
  `ishl`/`sshr`/`ushr` mask the count to the operand width, which is the same rule, so shifts are
  emitted directly. `%` on floating-point operands is declined until the runtime has `fmod`.
  Tests: `tests/native_codegen_e2e.rs`
  (`integer_arithmetic_follows_kotlin_where_the_machine_traps_or_wraps_differently`).

- **`==` on references is declined, not emitted as C's `==`.** Kotlin's `==` is `equals`; C's
  compares addresses. A structural-equality runtime does not exist yet, and emitting the address
  comparison would compile, link, run and answer a different question. `x == null` and `===` ARE
  address comparisons in Kotlin, and those are emitted as one.
  Tests: `tests/native_codegen_e2e.rs`
  (`structural_equality_on_references_is_declined_rather_than_compared_by_address`).

- **`==` on references is `equals`, dispatched through the receiver.** Kotlin's `==` is
  `a?.equals(b) ?: (b === null)`, which is exactly what the runtime's `kt_equals` does: null-safe,
  then a call through the receiver's vtable, so an overridden `equals` answers for itself and a
  class without one falls back to identity. Two separately built strings with the same text are
  equal, and a scalar on either side boxes, because `any == 5` means `any?.equals(5)` there too.
  Tests: `tests/native_codegen_e2e.rs` (`structural_equality_on_references_asks_the_receiver`,
  `a_null_check_and_identity_stay_pointer_comparisons`).

- **Boxing a small value hands out the same object every time.** A program can observe box
  identity — `boxBoolean(true) === boxBoolean(true)` is true in Kotlin — because the JVM and
  Kotlin/Native both cache small boxes. The runtime caches the range the JVM specifies: every
  `Byte`, `Short`/`Int`/`Long` in -128..127, `Char` in 0..127, and both `Boolean`s. The cache is
  static storage, which both keeps it alive across every collection and costs the collector
  nothing: a candidate address that resolves to no heap chunk is dropped, by the conservative root
  scan and the precise field tracer alike. Outside the range a box is a fresh object and identity
  is unspecified, as Kotlin says.
  Tests: `tests/native_codegen_e2e.rs` (`boxing_a_small_value_hands_out_the_same_object`).

- **`x!!` is a runtime check, and on a nullable primitive it is the unboxing.** The check lives in
  the runtime (`kt_not_null`) so the failure reads the same whatever produced the null, and so the
  generator emits no branch for what is, on every path that matters, a value passing through.
  Kotlin throws a `NullPointerException` here; with no exception machinery yet the honest
  realization is a diagnosable exit.
  Tests: `tests/native_codegen_e2e.rs`
  (`a_not_null_assertion_passes_a_value_through_and_fails_on_null`).

- **A `when` whose arms disagree on a carrier is a reference.** `when (s) { "a" -> 1; else -> null }`
  is `Int?`: taking the first arm's type would carry it as `Int` and store the `null` arm's pointer
  into a 32-bit slot. An arm that leaves (a `return`) has no type and does not vote.
  Tests: `tests/native_codegen_e2e.rs`
  (`a_when_whose_arms_have_different_types_is_carried_as_a_reference`).

- **An array is one object shape, and what varies is its type descriptor.** A header, a length,
  then the elements; the descriptor says how wide an element is and whether the collector should
  look inside. So `IntArray` and `Array<String>` are the same shape, every array type belongs to
  the RUNTIME rather than to a program (an array's descriptor depends on the element WIDTH, not on
  the element type someone wrote), and a `LongArray` is never walked as pointers even though its
  elements are pointer-width.
  **What an array stores is not always what its element type says**: `Array<Int>` holds boxed
  elements while `IntArray` holds the integers, so a read converts from the stored carrier to the
  type the operation declares — the boundary the JVM crosses with a `checkcast` and an
  `intValue()`. **Every access is bounds-checked**, compared unsigned so one comparison catches a
  negative index too; Kotlin throws `IndexOutOfBoundsException` and, with no exception machinery
  yet, the honest realization is a diagnosable exit. `==` on arrays is identity, as Kotlin says, so
  array types take `kotlin.Any`'s vtable rather than the built-in value one.
  Tests: `tests/native_codegen_e2e.rs` (`arrays_read_write_and_know_their_size`,
  `an_array_index_outside_its_bounds_fails_loudly`,
  `a_reference_array_is_traced_through_collection`,
  `a_reference_array_of_a_primitive_boxes_at_the_element_boundary`).

- **`Unit` is a value, and it is the runtime's.** A position that wants a reference — `val u: Any =
  Unit`, an `Any?` argument, a `Unit`-returning lambda's result — gets the runtime's singleton, so
  there is one `Unit` program-wide and a file that merely mentions it emits nothing.
  Tests: `tests/native_codegen_e2e.rs` (`the_unit_value_is_the_runtimes_own`).

- **A function value is an object, and calling one is a vtable dispatch.** Common lowering has
  already made a lambda's body a top-level function whose LEADING parameters are the captured
  values, so what remains is an object holding those captures, with its body in the vtable slot
  after `kotlin.Any`'s three. The thunk in that slot takes and returns references and converts at
  the boundary — Kotlin's own `FunctionN.invoke` convention, and for the same reason: a call site
  knows the arity but not which lambda it holds, so every function value of an arity has to be
  callable one way. A `Unit`-returning lambda answers with the runtime's `Unit`.
  **A lambda that captures nothing is one object**, not one per evaluation — `{}` has the same
  `hashCode` every time, which a program can see — and with no fields to hold it needs no
  allocation at all, so it lives in static storage. One that does capture is a fresh object each
  time, because each holds its own values, and those are traced through its descriptor like any
  other field.
  Tests: `tests/native_codegen_e2e.rs`
  (`a_lambda_is_an_object_that_can_be_passed_called_and_returned`,
  `a_lambda_that_captures_nothing_is_one_object`,
  `a_function_value_survives_collection_with_its_captures`).

- **A captured `var` is one cell, shared — and the cell is what is carried, whatever the
  declaration says.** Common lowering boxes a mutable local that a closure captures into a holder
  and rewrites its reads and writes to go through it, so the closure and the frame that made it see
  each other's writes rather than a copy. That holder is a one-field object here, traced when the
  field holds a reference. The declaration is NOT what says so: a local declared `var n: Int` keeps
  `Int` on its `Variable` node and on the lambda body's parameter, because `Int` is what the
  programmer wrote, while what is stored and passed is the cell. The initializer settles the local
  (a holder is created by `RefNew`) and the body settles the parameter (it reaches a holder through
  `RefGet`/`RefSet` rather than using the value). Believing the declaration truncates a pointer into
  a 32-bit slot — a miscompile with no symptom where it happens, which is why it took a program that
  kept something alive across a collection and read it back to see it.
  Tests: `tests/native_codegen_e2e.rs` (`a_lambda_captures_a_mutable_local_by_reference`);
  `tests/native_gc_stress_e2e.rs` (`closures_and_their_captures_survive_collection`).

- **Concurrency: there is none yet, and that is a tested claim rather than an omission.** The
  runtime's whole kernel interface is `write`, `mmap`, `munmap` and `exit_group`; nothing creates
  anything that runs concurrently, so the collector stops nothing. `@Volatile` therefore compiles as
  an ordinary property — with one thread its meaning is exhausted, and there is no observer for it
  to be wrong for — and a `suspend` function that never actually suspends is an ordinary function,
  because `suspend` is a calling convention rather than concurrency. A function that DOES suspend is
  declined: it needs a state machine to resume into, and compiling it into a straight call would run
  a coroutine body to its first suspension and silently carry on.
  Tests: `tests/native_concurrency_e2e.rs` — including one that reads the syscall header, so adding
  `clone` fails there before the Kotlin/Native memory model this target committed to is implemented.

- **A companion object is initialized when its class is first constructed.** That is the moment the
  JVM would have run the class's `<clinit>`, and what a `<clinit>` does for a class with a companion
  is create the companion instance, running its initializers — so they run before the constructed
  class's own, once, and not at all until something constructs the class. Construction is the only
  such trigger the generator can see; the JVM's other one, a class-static call, is declined by name.
  Tests: `tests/native_classes_e2e.rs`
  (`a_companion_object_is_initialized_when_its_class_is_first_constructed`).

- **`===` between two primitives compares the values.** Kotlin defines identity equality on values
  of a primitive type as `==` (it warns that the distinction is meaningless there). Boxing each
  side and comparing the boxes' addresses would say `0L !== 0L`, which the corpus caught. Identity
  on floating-point values (`-0.0`, `NaN`) is declined until the runtime pins its rules.
  Tests: `tests/native_codegen_e2e.rs` (`identity_equality_on_primitives_compares_values`).

- **The unsigned integers are carried by the checked type, not by the bits.** Common lowering
  erases each of the four value classes to the signed machine integer it wraps, which is right
  about the REPRESENTATION and silent about how to read it: `4294967295u` and `-1` are the same 32
  bits. The generator recovers the difference from `ir.logical_types`, the checked type beside each
  expression, and four decisions follow from it.

  * **The carrier keeps the signedness.** `Carrier::Scalar(clif::Type, signed)` answers *unsigned*
    for the four, so every widening zero-extends and every ABI parameter is `uext`. A member the
    machine already answers the same way either way — `plus`, `minus`, `times`, the bitwise
    operators, `inv` — is the signed instruction, because two's complement makes the bits identical;
    a member where the machine offers both and the choice is the point — `compareTo` and the
    orderings, `div`, `rem` — takes the unsigned one; `shr` is a LOGICAL shift, since the top bit is
    a value. A member named by neither declines by name rather than falling back to the signed
    answer, which is why the family was declined whole before this.
  * **`UByte` and `UShort` are narrowed where the checked type says so.** Both reach the generator
    already widened into an `Int` — `UShort.MAX_VALUE` arrives as `Const(Int(-1))` — so a value
    whose checked type is narrower than the bits carrying it is `ireduce`d to its own width before
    anything reads it. Only where the physical carrier is a scalar: the same expression boxed is a
    reference, and reducing a pointer is not a narrowing.
  * **Each has its own runtime descriptor.** `kt_type_ubyte`/`ushort`/`uint`/`ulong` sit beside the
    signed ones, so a boxed `1u` answers `is UInt` and not `is Int`, `1u as? Int` is `null`, and
    `"$any"` renders the value rather than the sign. Structural equality and `hashCode` group each
    unsigned descriptor with the signed field it shares — the descriptors have already been required
    to match, so the bits decide equality, and Kotlin defines each unsigned `hashCode` as the
    wrapped value's.
  * **`toString` leaves the machine.** `kt_ubyte_to_string` and its three siblings render through an
    unsigned division loop; the signed renderer negates into unsigned space to reach the digits,
    which is exactly the step that must not happen here.

  Tests: `tests/native_unsigned_e2e.rs` (the whole file), `tests/native_codegen_e2e.rs`
  (`an_unsigned_integer_is_not_the_signed_one_sharing_its_bits`).

- **Arithmetic on `Byte`/`Short`/`Char` produces `Int`.** Kotlin has no `Byte.plus(Byte): Byte`, so
  the operands of a built-in arithmetic operator on a narrow integer type are widened to `i32`
  first (sign-extended, or zero-extended for `Char`) and the result is an `Int`. Only `Char` against
  `Char` compares unsigned; widened to `Int` it is non-negative and a signed compare says the same.
  Tests: `tests/native_codegen_e2e.rs`
  (`integer_arithmetic_follows_kotlin_where_the_machine_traps_or_wraps_differently`, `narrow`).

- **A loop is four blocks — header, body, update, exit — and `break`/`continue` are edges.** A
  labeled `break` targets the named loop's exit block and `continue` its update block (the header
  when there is no update), so a lowered `for`, whose step is a statement sequence carrying its own
  overflow guard, runs that sequence at the `continue` target and its guard's labeled `break` finds
  the loop it names while the update is still being lowered. An `if`/`when` is a chain of
  conditional branches into one merge block; when it is used as a value the merge block carries it
  as a block parameter. An arm or a loop body that leaves (`return`, `break`, `continue`) simply
  contributes no edge, and whatever the IR still puts after it lands in a block nothing reaches.
  Tests: `tests/native_codegen_e2e.rs` (`arithmetic_locals_and_control_flow_run`,
  `a_function_call_and_recursion_run`).

- **An unsupported construct declines the whole file with a diagnostic.** The JVM backend can afford
  a best effort because `kotlinc` decides what is correct; nothing decides that for native yet, so a
  partial emission would produce a program that links and misbehaves.
  Tests: `tests/native_codegen_e2e.rs` (`an_unsupported_construct_is_declined_with_a_diagnostic`).

- **Every heap object begins with its type, and memory is reclaimed by a mark-sweep collector whose
  roots are conservative and whose heap tracing is precise.** A `KType` descriptor names the byte
  offset of every reference-typed field, and the collector follows exactly those; a `Long` field
  holding a pointer's bits does not keep anything alive. Roots — the stack and the callee-saved
  registers — are the one place scanned conservatively, because the code generator emits no stack
  maps yet: any stack word that points into an allocated object
  (interior pointers included) roots it. Global slots are roots only when registered
  (`kt_gc_add_global_root`); static storage is never scanned and never treated as an object.
  Collection is triggered by allocation volume since the last collection (never by heap size, which
  would double the heap each cycle for a program with a small live set) and can be forced with
  `kt_gc_collect()`.
  Tests: `tests/native_gc_e2e.rs` (`the_collector_reclaims_garbage_and_keeps_what_is_reachable`,
  whose C program pins precise heap tracing by an object referenced only from a `kt_long` field
  being reclaimed, and interior-pointer rooting, cycle reclamation, slot reuse, large-object
  unmapping and the automatic trigger);
  `tests/native_codegen_e2e.rs` (`a_program_that_allocates_heavily_runs_under_collection`).

- **The collector never moves an object.** A conservative root cannot be updated — the word that
  looks like a pointer may be an integer — so nothing that a root might refer to may change
  address. That forecloses compaction and a copying nursery until krusty owns its code generator and
  can emit stack maps (`docs/BUILD_AND_NATIVE_PLAN.md`, *Decided: krusty owns its runtime*, step 3).
  The heap is shaped accordingly: size-segregated chunks, a freed slot reused in place, a large
  object in a mapping of its own that is returned to the kernel when it dies.
  Tests: `tests/native_gc_e2e.rs` (rooted objects are read back at the same address with the same
  contents after a collection; a second batch the size of a freed first batch maps nothing new).

- **A `String`'s text is a heap object of its own — a byte array with no reference fields — and the
  string's type lists the field holding it as a reference.** The text therefore lives exactly as
  long as some string uses it, and two strings may share one array. A literal is different: its
  bytes stay in static storage and the string's array field is `NULL`, so the collector has nothing
  to trace and nothing to free. Rendering a number or a `Char` allocates a byte array the same way;
  the runtime keeps that array in a local across any further allocation, which is what makes it a
  root.
  Tests: `tests/native_gc_e2e.rs` (a string built across repeated collections prints intact, and a
  literal and `Unit` pass through a collection untouched);
  `tests/native_codegen_e2e.rs` (`a_program_that_allocates_heavily_runs_under_collection`).

- **A class instance is a header followed by the superclass's fields, then its own.** The object
  header stays one word — the collector's contract is `header->type` and nothing here changes it.
  Fields follow in the classic single-inheritance layout: the superclass's fields as a prefix in the
  superclass's order, then this class's in declaration order, each aligned to its size
  (`Boolean`/`Byte` 1, `Short`/`Char` 2, `Int`/`Float` 4, `Long`/`Double`/reference 8), and a
  subclass's first field packed directly after the superclass's last (not after its rounded size).
  The instance size rounds up to 8. One computation of the layout feeds both the loads and stores
  the code generator emits and the `reference_offsets` table in the class's descriptor, so the
  program and the collector cannot disagree about where a reference is.
  Tests: `src/native/classes.rs` (`fields_follow_the_superclass_prefix_and_align_to_their_size`,
  `a_subclass_field_packs_after_the_superclass_field_not_after_its_rounded_size`);
  `tests/native_classes_e2e.rs`
  (`a_three_level_hierarchy_inherits_fields_and_overrides_at_each_level`,
  `a_linked_chain_built_under_collection_pressure_is_traced_through_emitted_layouts` — the test that
  catches a wrong `reference_offsets`).

- **Virtual dispatch goes through the type descriptor, and every vtable begins with `kotlin.Any`'s
  three slots: `equals`, `hashCode`, `toString`, in that order.** A call is
  `obj->type->vtable[slot]`: one indirection more than a vtable pointer in the header, in exchange
  for leaving the header — and the collector — untouched. A class's table is its superclass's with
  overridden slots replaced and new members appended, so a slot assigned at the declaring class is
  valid down the whole hierarchy. Open properties are members too: an `open`/`override` property
  dispatches through a getter (and, for a `var`, a setter) slot, synthesized as a field access when
  the source wrote no accessor. `super.f()` is a direct call to the named class's body. An abstract
  member's slot is a runtime function that fails loudly rather than a NULL to jump through. An
  override that changes a parameter's or result's machine representation (`T` specialized to `Int`)
  would need a bridge method and is declined by name.
  Tests: `src/native/classes.rs` (`a_new_method_appends_and_an_override_replaces_the_slot`,
  `a_three_level_chain_keeps_slot_numbers_stable`,
  `an_override_that_changes_representation_is_declined`); `tests/native_classes_e2e.rs`
  (`a_call_through_a_base_typed_value_reaches_the_override`,
  `a_super_call_runs_the_base_implementation_then_the_override`,
  `an_abstract_method_dispatches_to_each_implementation`,
  `an_open_property_read_through_the_base_type_reaches_the_override`).

- **The default `hashCode` derives from the object's address, and the default `toString` is
  `<qualified name>@<hex hashCode>`.** An identity hash from the address is legitimate only because
  this collector never moves an object — conservative roots forbid it — so the address is stable
  for the object's whole life. `equals` defaults to reference identity. A user `toString` is reached
  from `println(obj)`, from `"$obj"` and from an explicit call alike, because the runtime's
  rendering dispatches through slot 2 for anything that is not one of its own value types; the
  built-in values compare by value and hash as Kotlin specifies (a `String` over its UTF-16 code
  units). Structural `==` between references stays declined (see above); `x == null` is emitted as
  the pointer comparison Kotlin defines it to be.
  Tests: `tests/native_classes_e2e.rs`
  (`a_user_to_string_is_reached_through_println_templates_and_explicit_calls`).

- **`is` walks the superclass chain; a failed `as` fails loudly, as the placeholder for
  `ClassCastException`.** `obj is T` is false for `null` and true when `T`'s descriptor is on the
  object's `super` chain; `as?` yields the object or `null`; `as T?` and a compiler-inserted smart
  cast let `null` through; `as T` to a non-null type fails on `null`. There are no exceptions in the
  runtime yet, so a failed cast exits with a message naming both types — the same realization
  unboxing `null` already uses — rather than passing the object through and letting the program read
  a subclass's fields off a base object. A check against a type that is neither a class of the file
  nor a runtime value type declines.
  Tests: `tests/native_classes_e2e.rs` (`is_and_safe_casts_follow_the_superclass_chain`,
  `a_failed_cast_fails_loudly_naming_both_types`).

- **An array type is one of those runtime types, and what its descriptor separates is the element
  WIDTH.** `is`, `as` and `as?` against `IntArray` or `Array<*>` ask the runtime about the same
  descriptor an allocation already stamps on the array, because the collector has to be told the
  element width and whether to look inside. That width is exactly Kotlin's own erasure here:
  `Array<String>` and `Array<Foo>` are one type, `IntArray` is neither of them, and `is Array<*>` is
  the only form a program may write. An `as` to an array is therefore CHECKED like a class's, where
  before it fell through to a coercion — which changes nothing about a reference and so let an
  `Array<String>` be read as an `IntArray`, four bytes out of every eight-byte slot. An unsigned
  array still declines: the runtime lays out no array for it, so there is no descriptor to name.
  Tests: `tests/native_type_checks_e2e.rs`, and `tests/native_classes_e2e.rs`
  (`a_failed_cast_to_an_array_fails_loudly_too`).

- **A cast is CHECKED whenever its target is held as a reference and the runtime names it; a scalar
  target is a representation change.** The rule is not about arrays: a cast with no descriptor to
  check against fell through to a coercion, and a coercion changes nothing about a reference — so
  `(1 as Any) as String` handed the box back typed `String` and `length` read a field off the wrong
  object. `String` and an array are the runtime's types rather than the program's, and both are now
  asked about exactly as a declared class is. `x as Int` stays a coercion, and deliberately: it is
  an unboxing, and routing it through the object check would answer with the box where the site
  wants the number. A target the runtime does not name — a dependency's interface, a type parameter
  — still coerces, which is Kotlin's own erasure.
  Tests: `tests/native_type_checks_e2e.rs` (`a_string_is_asked_about_the_same_way_a_class_is`,
  `an_unboxing_cast_stays_a_representation_change`), `tests/native_classes_e2e.rs`
  (`a_failed_cast_to_a_string_fails_loudly_too`).

- **An annotation on a declaration is metadata this target does not keep; an annotation INSTANCE is
  a value it declines to build.** Nothing emitted for this target can be asked what annotations a
  declaration carries, so applying one changes nothing a program can observe and the declaration
  costs the generator nothing to accept — every target a program may write one on, a class, a
  function, a property, a field, a parameter, a local and an expression. Constructing one declines,
  and the line is at the value rather than at each thing done with it: Kotlin defines an annotation
  instance's `equals`, `hashCode` and `toString` over its arguments, with array members compared and
  rendered by CONTENT, and this backend would give it `kotlin.Any`'s identity ones. Reading a
  member would then answer correctly while comparing two instances answered `false` where Kotlin
  says `true` — a half-right that answers rather than declines. (The JVM backend realizes these
  members through an `annotationImpl` class it synthesizes itself, so the shared lowering hands a
  backend the annotation class directly; making the instance work here is therefore a question for
  the common lowering rather than a second copy of Kotlin's rule.)
  Tests: `tests/native_annotations_e2e.rs`.

- **Two `is` checks are settled by the type alone, and one more is the runtime's `Unit`.** `Nothing`
  has no instances, so `x is Nothing` is false and `x is Nothing?` is exactly `x == null`; every
  non-`null` value is an `Any`, so `x is Any` is `x != null` and `x is Any?` is true of everything.
  The receiver is still evaluated: the constant is the answer, not the expression. `Unit` reaches a
  check spelled as the object it is rather than as the carrier the generator names, and answers
  through the one descriptor either way.
  krusty's JVM backend answers `is Nothing` and `is Unit` with `true` for every non-`null` value —
  `ref_internal` has no arm for either, so it emits `instanceof java/lang/Object`. That is a defect
  in shared code, fixed on its own branch; the native tests for these two shapes assert this backend
  only until it lands, rather than pinning the wrong answer.
  Tests: `tests/native_type_checks_e2e.rs` (`nothing_is_a_type_no_value_is_an_instance_of`,
  `any_is_the_question_of_whether_there_is_a_value_at_all`,
  `a_settled_check_still_evaluates_its_receiver`, `unit_is_asked_about_as_the_object_it_is`).

- **`Number` and `Comparable` are descriptors the value types POINT AT, not types anything wears.**
  Neither has an instance of its own — every value that is one is a boxed primitive or a string — so
  each is a runtime descriptor named by the boxes' interface lists, which are flattened and
  transitive because `is` scans them at each step of the super chain rather than walking an interface
  hierarchy. Which box points at which is Kotlin's own asymmetry and not a rule about machine width:
  `Char` and `Boolean` are `Comparable` and not `Number`, and an unsigned integer is `Comparable` and
  not `Number` either, being a value class rather than a `java.lang.Number`. Every one of those
  answers is the reference compiler's, asked of it directly.
  Tests: `tests/native_builtin_supertypes_e2e.rs`.

- **A top-level property is a global slot, initialized before the entry function, and rooted in the
  collector if it holds a reference.** The JVM realizes a top-level property as a private static
  field plus a `getX`/`setX` pair (and an `access$get<X>$p` bridge when a sibling class reads a
  private one) because of JVM visibility rules; none of that applies natively, so a read is a load
  from the slot and a write is a store. An accessor is called only where the SOURCE wrote one
  (`val doubled get() = …`), which is exactly when common lowering emits an accessor function —
  the property's `field` reads inside it lower to the slot as any other read does. Initializers run
  in declaration order at process start, which is where the JVM would have run the facade's
  `<clinit>`: a program touches the facade by calling its entry point. A reference-typed slot is
  registered with `kt_gc_add_global_root` BEFORE the first initializer runs, because a later
  initializer may allocate and the collection that follows must already trace the earlier slots.
  Tests: `tests/native_codegen_e2e.rs`
  (`top_level_properties_initialize_before_main_and_hold_their_values`,
  `a_top_level_property_is_a_collector_root`,
  `a_top_level_property_with_custom_accessors_runs_their_bodies`).

- **An `object` declaration is one lazily constructed instance in a static slot registered as a
  collector root.** Static storage is never scanned, so the emitted getter registers the slot with
  `kt_gc_add_global_root` before it allocates and assigns the slot before running the constructor;
  whatever the singleton references then survives every collection. The test creates the singleton
  in a frame that is gone and overwritten before the churn, so only the registration keeps it alive
  — removing the registration makes the program fault.
  Tests: `tests/native_classes_e2e.rs`
  (`an_object_declaration_is_one_instance_rooted_across_collections`).

- **Initialization order is Kotlin's: superclass constructor first, then this class's parameter
  stores, then its property initializers and `init` blocks in source order.** The superclass
  constructor's arguments are evaluated from the derived constructor's parameters before the call.
  Slots are zeroed by the allocator, so a field read before its store observes `null`/`0`, as it
  does on the JVM.
  Tests: `tests/native_classes_e2e.rs`
  (`initialization_runs_the_superclass_first_then_fields_then_init_blocks`).

- **A generic class erases its type parameters to references.** `Box<T>(val value: T)` stores `T`
  as a `KRef`; a scalar argument boxes on the way in and unboxes on the way out, exactly as on the
  JVM.
  Tests: `tests/native_classes_e2e.rs` (`a_generic_class_erases_its_parameter_to_a_reference`).

- **`list += x` appends the ELEMENT; `list += xs` appends every element of `xs`. The operand is the
  LAST physical parameter, never the first.** Kotlin declares `plusAssign` once per shape of
  right-hand side, and the two spellings are identical at the call site, so only the operand's type
  separates them. Most of these declarations are EXTENSIONS, which carry their receiver as the first
  physical parameter — so reading the first asks whether the RECEIVER is a collection, which is
  always true, and `xs += 1` walks the integer as if it were one. A member has only the operand,
  where first and last coincide. The same rule tells `removeAt(index)` from `remove(element)`, and
  there it must read the PHYSICAL parameter rather than the semantic one: `MutableList<Int>` has
  substituted `E` to `Int`, so both overloads look like `remove(Int)` until the realization is
  consulted.
  Tests: `tests/native_lists_e2e.rs` (`plus_assign_appends_an_element_or_every_element`,
  `plus_assign_of_an_int_appends_it_rather_than_walking_it`),
  `tests/mapped_collection_scope_e2e.rs` (`remove_of_an_absent_element_is_not_an_index`).

- **A progression's last element is computed modulo the step, never from the distance between its
  bounds.** `first + ((last - first) / step) * step` is right for every range a program is likely to
  write and wrong for the widest: `Long.MIN_VALUE..Long.MAX_VALUE` spans more than a `Long` can
  hold, so the subtraction wraps and the walk stops after one element instead of reaching three.
  Reducing both bounds modulo the step never forms that distance — every intermediate stays inside
  `0 until step` — which is why Kotlin's own `getProgressionLastElement` is written this way.
  Tests: `tests/native_ranges_e2e.rs` (`a_progression_spanning_the_whole_range_reaches_every_step`);
  the corpus cases are `codegen/box/ranges/stepped/**/…StepMaxValue.kt`.

- **`StringBuilder` is a growable UTF-8 buffer whose `toString` COPIES, and whose `equals` and
  `hashCode` stay identity.** `substring` shares its receiver's storage because a string is a value
  and nothing can write through it; a builder can be written through, so a view handed out before an
  `append` would change under a program already holding it — and would change only sometimes, since
  a write within the current capacity rewrites bytes in place while a write past it moves them. The
  copy is what makes the snapshot a value. `equals`/`hashCode` are NOT overridden, here or on any
  Kotlin target: two builders holding the same text are different objects.
  Every question about a builder's CONTENT — `length`, `sb[i]`, iterating it — is answered by the
  same runtime entry points a `String`'s questions reach, which read the text through one accessor
  rather than off a string's fields. `append` renders its operand through the operand's own
  `toString`, which is the same answer for all dozen JVM overloads and is why they need one
  function. `appendLine` appends `\n` and not the host's line separator, which is what Kotlin
  specifies on every target. `sb.append(null)` is an overload AMBIGUITY in Kotlin — kotlinc rejects
  it too — so a bare `null` needs a type.
  The class is declared in no file krusty compiles, so constructing one takes the path `Any()` and
  `ArrayList()` already take: the runtime allocates it. `StringBuilder(capacity)` is a hint nothing
  observable depends on; `StringBuilder(text)` copies the text.
  Tests: `tests/native_string_builders_e2e.rs` (all).

- **An exception propagates through a PENDING SLOT, and every call checks it.** `throw` records the
  exception in one runtime slot and RETURNS the frame's zero value; every call site loads the slot
  and branches — to the innermost enclosing `try`'s dispatch block, or out of the frame with the
  slot still set, which IS the propagation. A caller never reads the value a throwing call
  returned, because it checks first. The slot is a GC root: between the throw and the `catch` that
  names it the exception is reachable from no frame.
  The slot is READ, not fetched through an accessor. A call clobbers the caller-saved registers, so
  one after every call doubles what a frame keeps alive across a call boundary — and the frames
  grow. The corpus priced that exactly: a 100,000-deep recursion overflowed its stack when the
  check was a call and does not when it is a load. One load and one predicted branch is also what
  the design was costed at.
  EVERY call in a function body carries the check, including the DISPATCHED one, which is the only
  call this backend emits outside the shared helper — a `try` whose body invokes a lambda is the
  common shape, and the exception walked straight out of the `try` until that call checked too.
  The handler machinery itself must NOT check: deciding which clause takes the exception happens
  while one is pending by construction, so a check there reads the very slot being examined, finds
  it set, and leaves — every `catch` then swallows nothing.
  Clauses are tried in source order, which is Kotlin's and the JVM's, by `kt_is_instance` against
  each clause's type; a clause that matches CLEARS the slot before running, because from there the
  exception is handled and the handler's own calls check that slot like any others. No clause
  matching falls through to where the exception was already going.
  Tests: `tests/native_try_catch_e2e.rs` (all).

- **`finally` runs on FOUR exit edges, and an exception raised inside one LEAVES the `try` it
  belongs to.** The edges are: the body completing, each handler completing, an exception no clause
  matched, and a `return`, `break` or `continue` written inside. The block is re-lowered on each —
  which is what kotlinc emits too, since the paths are disjoint and a copy on each runs once.
  A `try` with a `finally` therefore needs a SECOND handler above clause selection: an exception
  raised in a `catch` clause is not offered to that same `try`'s other clauses, but must still run
  the `finally` on the way out. And the `finally` itself is lowered at the handler depth its `try`
  was ENTERED at, because an exception raised inside a `finally` leaves that `try` — without that,
  a throwing `finally` arrives back at its own dispatch and runs a second time
  (`codegen/box/finally/breakAndOuterFinally.kt`, which logged `… finally finally`).
  On the propagating edge the pending slot is CLEARED and the exception held in a local, because
  the block's own calls each check that slot and the first would otherwise turn straight round.
  Afterwards the exception resumes unless the `finally` outranked it. Every rule here came from
  kotlinc 2.4.10 rather than from reading the construct: a `finally` that THROWS replaces the
  exception in flight, one that RETURNS swallows it, a `return` in a `finally` beats a `return` in
  the body, nested finallys run innermost first, and a `break` out of a `try` runs its `finally` on
  the breaking turn.
  A `break` runs the finallys entered INSIDE the loop it leaves and no others, which is what the
  loop depth recorded at each `try` is for.
  Tests: `tests/native_try_catch_e2e.rs` (the ten `finally` cases).

- **The runtime's own failures are Kotlin exceptions a program can catch.** Each was a diagnosable
  exit while no handler could exist to see the difference; each is now the exception Kotlin
  specifies, raised through the same slot a written `throw` uses, so a clause cannot tell the two
  apart. A failed cast is `ClassCastException` reading `class A cannot be cast to class B`, which
  is Kotlin/Native's wording; `x!!` is `NullPointerException` with NO message and `null as T` is
  one whose message names the target type — kotlinc 2.4.10 confirms both, and conflating them is
  the easy mistake, since a null is not an instance of anything and there is no class to report as
  a cast's source. Division by zero is `ArithmeticException("/ by zero")`, an index outside an
  array is `IndexOutOfBoundsException`, `valueOf` of an unknown constant is
  `IllegalArgumentException`, a non-positive `step` is `IllegalArgumentException`, and writing
  through a list while walking it is `ConcurrentModificationException` — decided by a MODIFICATION
  COUNT rather than the size, because `remove` during a walk can leave the cursor inside the
  shortened list where the bound says nothing is wrong.
  Every one of these RETURNS after raising, and every caller must act on that: the exception is
  recorded, not raised, so falling through reaches the machine divide or the out-of-range read the
  check was there to avoid. Division by zero took a SIGFPE until each site returned.
  Tests: `tests/native_try_catch_e2e.rs`
  (`the_runtimes_own_failures_are_catchable_kotlin_exceptions`,
  `a_failed_cast_names_both_classes_the_way_kotlin_native_does`,
  `a_null_cast_is_a_null_pointer_exception_naming_the_target_type`, `valueof_of_an_unknown_constant_throws`),
  `tests/native_lists_e2e.rs` (`writing_through_a_list_while_walking_it_is_a_concurrent_modification`).

- **A cast whose target is a primitive is still a question about the object, unless the source is
  already that same primitive.** `(1 as Any) as Byte` is a `ClassCastException` in Kotlin: the
  widths are convertible and the TYPES are not. So is `it as Byte` where `it` is a type parameter
  the call substituted to `Int` — and that one arrives with both sides already unboxed, because
  Kotlin has no cast between two primitive types (`val x: Int = 1; x as Byte` does not compile), so
  a scalar-to-other-scalar cast can only have come from erasure. Both ask the descriptor before
  reading anything out; unboxing and converting first answers `1` to a program Kotlin refuses.
  Where there is no descriptor to test against — an erased type parameter — the NON-NULL-ness of
  the cast is still checked, and the node's own operation is what decides it, never the spelling of
  the target: an unbounded `T` is not nullable as a type and `null as T` is still legal, because
  `T` may be instantiated with a nullable one. Reading the target instead made eight programs throw
  that Kotlin accepts.
  Tests: `tests/native_try_catch_e2e.rs`
  (`a_cast_between_two_primitives_can_only_be_an_erased_object_cast`); the corpus cases are
  `codegen/box/casts/{asForConstants,asWithGeneric,castToDefinitelyNotNullType,kt59022}.kt`.

- **A `lateinit` read is guarded wherever a field is LOADED, not wherever one is written in the
  source.** There are four paths that load a field — the `GetField` node, a property read that
  finds storage rather than a getter, the `super` read that does, and the synthesized accessor a
  property reached through a vtable slot arrives at — and a property that OVERRIDES another takes
  the last of them. Putting the guard on the load is what makes one rule serve all four. Kotlin
  puts it at the read rather than tracking initialization because the field being null IS the
  evidence, which is also why `lateinit` is confined to types that have a null.
  A TOP-LEVEL `lateinit var` is NOT guarded, and that is common lowering's gap rather than this
  backend's: its getter carries no check and krusty's JVM backend answers null there too
  (`codegen/box/properties/lateinit/topLevel/`, both ledgered).
  Tests: `tests/native_try_catch_e2e.rs` (`reading_a_lateinit_property_before_it_is_set_throws`,
  `a_lateinit_property_that_overrides_one_is_guarded_too`).

- **`CharSequence` is a descriptor the text types point at, and a cast to a type PARAMETER is a
  cast to its bound.** `CharSequence` has no instances of its own, so it takes the arrangement
  `Number` and `Comparable` already use — except that TWO types point at it, a `String` and a
  `StringBuilder`. Answering the question with the string's own descriptor would have been sound
  while a string was the only text this runtime made, and stopped being sound the moment there was
  a builder.
  A bound is all that is left of a type parameter at run time, and it is exactly what kotlinc
  checks: `fun <T : CharSequence> f(x: Any?) = x as T` rejects a non-`CharSequence` inside `f`,
  before the call site's own cast to the argument it was given. An unbounded parameter bounds at
  `Any?`, where there is nothing to check.
  Tests: `tests/native_string_builders_e2e.rs` (`both_text_types_answer_to_char_sequence`),
  `tests/typeparam_cast_e2e.rs` (`class_bounded_type_param_cast_checkcasts`, which cross-checks the
  two backends).

- **`assertFailsWith<T> { … }` reads its expected class from the call's RETURN type, and a wrong
  throw FAILS the assertion rather than propagating.** The reified `T` never reaches a backend as a
  type argument — kotlinc resolves it into the return type — so there is no class operand to find,
  and the descriptor that type already wears is the test. It is the same `kt_is_instance` a `catch`
  clause makes, so a SUPERTYPE matches: `assertFailsWith<RuntimeException>` takes an
  `IllegalStateException`. The block is the LAST argument, never the first: `message` is declared
  before it and defaulted, so a call that omits it passes one argument and a call that supplies it
  passes two.
  A block that throws the WRONG type raises `AssertionError` and the original exception is
  REPLACED, not allowed past — kotlin-test catches `Throwable` and fails on what it caught.
  Letting it propagate is the plausible reading and it is wrong; kotlinc 2.4.10 settled it, along
  with the wording of both failures and the `". "` a supplied message is joined by.
  Tests: `tests/native_try_catch_e2e.rs` (the five `assert_fails_with*` cases).

- **A boxed floating-point value compares and hashes by CANONICALIZED bits: every NaN is one
  value.** This is `java.lang.Double.doubleToLongBits`, and the difference from
  `doubleToRawLongBits` is the whole point — `0.0 / 0.0` produces a NaN with the sign bit SET on
  x86 (`fff8…`) where the `Double.NaN` constant does not (`7ff8…`), so comparing raw bits answers
  false for two values Kotlin calls equal. `hashCode` canonicalizes through the same helper,
  because two values that compare equal must hash equal and two NaNs do compare equal.
  (`0.0.equals(-0.0)` stays FALSE: those differ in a bit that is not a NaN payload. The scalar
  comparison emitted for `==` is a third rule again, where `0.0 == -0.0` is true.)
  Tests: the corpus case is `codegen/box/arithmetic/division.kt` (`assertEquals(Double.NaN, 0.0 / 0.0)`).

- **A branch's type is read from the IR, not from what lowering has emitted so far.** A `when`
  types itself BEFORE lowering any arm, because its merge block has to know whether it carries a
  value. A local's type was answered from the slot map, which is a lowering artifact — a slot is
  in it once its declaring statement has been emitted — so a branch ending in a local that branch
  DECLARES had no type. The `when` then typed as no-value, every arm was lowered as a statement,
  and whatever the branch computed went nowhere: the destination read zero.
  That is the shape every inline function spliced into a branch takes, which is how
  `if (c) Array(2) { … } else Array(5) { … }` answered an array of size 0 while the same
  constructor answered 2 outside a branch. The declaring `IrExpr::Variable` carries the type, so
  the question goes there.
  Tests: `tests/native_codegen_e2e.rs`
  (`a_branch_ending_in_a_local_it_declares_still_has_a_value`, which cross-checks the two backends).

- **A class nested in the FILE FACADE is not qualified by it on this target.**
  `castAnonymousClassKt$box$1` is the JVM's binary name for an anonymous object inside a top-level
  `box`, and it is right there — but there is no facade class here at all: a top-level property is
  a global and a top-level function is a symbol, neither owned by anything. Kotlin/Native names
  that object `box$1`, and that is what a failed cast reports. Only the PACKAGE separator becomes a
  dot: a `$` is Kotlin's own nesting separator and stays one, so a class local to `box` reads
  `box$MyLocalObject` rather than as a package that does not exist.
  Tests: the corpus cases are `codegen/box/casts/nativeCCEMessage/` (all four).

- **A collection literal's `operator fun of` dispatches on the companion, not on nothing.**
  `val list: MyList = ["O", "K"]` selects `MyList.Companion.of`, which is an ordinary MEMBER of
  that companion object — the same declaration a spelled `MyList.of("O", "K")` reaches. The
  literal spells no receiver, so the selected member reached the backend with no dispatch
  receiver recorded and the call was emitted with the arguments alone: one value short of the
  declaration it had selected, which the native code generator's verifier refused outright
  (`mismatched argument count … got 1, expected 2`). Selection now records the singleton on the
  committed member whenever the selected owner is an `object`, which covers a companion, a
  companion block, and an `object` that declares `of` on itself. A companion EXTENSION keeps its
  own path — it already carries the receiver it extends — and an implicit classifier callable
  (`values`/`valueOf`) has no instance at all and keeps none.
  Tests: `src/fir/body_check/collection_literal_tests.rs`
  (`custom_collection_literal_keeps_the_selected_companion_operator`),
  `tests/native_codegen_e2e.rs`
  (`a_collection_literal_calls_its_companion_operator_on_the_companion`); the corpus cases are
  `codegen/box/collectionLiterals/{genericCollection,multipleOfOverloads,nonGenericCollection,resolvesToOperator}.kt`.

- **A `return` converts into the result its declaration names.** A value typed by a type PARAMETER
  is carried as a reference whatever that parameter is bounded by, so `fun <T : Int> foo(x: T): Int
  = x` hands a pointer back where the declaration says a machine integer. The scalar `return` arm
  emitted the value untouched and the code generator's verifier refused the function outright
  (`result 0 has type i64, must match function signature of i32`). The conversion is an unboxing,
  and naming it needs the result TYPE rather than its carrier — `Boolean` and `UByte` share one
  carrier and unbox through different descriptors — so the body lowering now carries both.
  Tests: `tests/native_codegen_e2e.rs`
  (`a_result_declared_narrower_than_the_value_it_returns_is_converted`); the corpus case is
  `codegen/box/boxing/boxing15.kt`.

- **An override answering `Unit` still answers the base a value.** `open fun foo(): Any` overridden
  by `override fun foo(): Unit` changes the result's representation, so the base's vtable slot
  holds a bridge. The override's slot produces no machine value and the bridge returned nothing at
  all, where the base's signature promises a reference. `Unit` is a real Kotlin value — a caller
  going through `A` reads the singleton and compares equal to `Unit` — and the runtime owns the one
  instance of it, so the bridge materializes it rather than returning empty-handed.
  Tests: `tests/native_codegen_e2e.rs`
  (`an_override_that_answers_unit_still_answers_the_base_a_value`); the corpus case is
  `codegen/box/bridges/test18.kt`.

- **`ProperIeee754Comparisons` survives a boxed operand.** Kotlin compares two floating-point
  operands by IEEE rules whenever both static types are the floating-point type itself, INCLUDING
  through a type parameter bounded by it (`fun <T : Double> f(d: Double, v: T) = d == v`) and
  through its nullable form. `NaN == NaN` is then false and `0.0 == -0.0` true — and both verdicts
  reverse once an operand widens to `Any`, where `equals` answers the total order instead. A
  reference-carried operand is a BOX, not a widening, so the rule still applies to it: the
  comparison opens the box and compares the numbers rather than handing the pair to the runtime's
  `kt_equals`, which would answer the total order and be wrong rather than imprecise. `null` is not
  a number and is answered before any unboxing — a null equals only another null — while a scalar
  operand cannot be null and is asked nothing.
  Tests: `tests/native_codegen_e2e.rs`
  (`a_floating_point_comparison_stays_ieee_when_an_operand_arrives_boxed`, which cross-checks the
  two backends and REQUIRES the native lowering); the corpus cases are
  `codegen/box/ieee754/{equalsNaN_properIeeeComparisons,whenNullableSmartCast,when_properIeeeComparisons}.kt`,
  `codegen/box/binaryOp/{eqNullableDoublesWithTP,kt44402}.kt` and
  `codegen/box/regressions/kt71119.kt`.

  Known separately: the JVM backend emits a `VerifyError` for a call passing `null` to a
  `<A : Double?>` parameter (`eqNullableDoublesWithTP`'s shape), which is why the cross-checked
  program above stops at the nullable LOCAL and the generic nullable parameter is covered by the
  corpus case on the native lane only.

- **A property implemented at another representation is bridged, in both directions.**
  `interface C { var size: Int }` implemented by `class B : C, A<Int>()` where `A<T>` declares
  `var size: T`: the interface's accessors carry a machine integer and the inherited ones carry a
  reference, so pointing the interface's program-wide number at `B`'s slot has a caller read that
  integer as a pointer. The number takes an accessor bridge wearing the interface's carrier
  instead, forwarding by DISPATCH so a further subclass's override is still reached. The same
  holds on the CLASS chain, where `class D : B() { override var size: Int }` replaces a base slot
  that carries a reference: the base keeps its slot and gets the bridge, and the override takes one
  of its own. The entry carries the two property TYPES rather than two declaration ids, because
  neither end need be a source accessor — a synthesized field access has no declaration to name.

  What makes the shape hard to see is that NO declaration in the file says the property dispatches:
  `A.size` need be neither `open` nor an override, and `B` overrides nothing — Kotlin asks for no
  `open` because the implementing class declares nothing. So a property any class in the file hands
  to an interface gets a slot on that ground alone.
  Tests: `tests/native_codegen_e2e.rs`
  (`an_interface_property_implemented_at_another_representation_is_bridged`, which cross-checks the
  two backends and REQUIRES the native lowering); the corpus cases are
  `codegen/box/bridges/{test3,test5,test7,test8,test15,genericProperty}.kt`,
  `codegen/box/basics/kt75483.kt`, `codegen/box/classes/kt6136.kt` and
  `codegen/box/properties/{primitiveOverrideDefaultAccessor,primitiveOverrideDelegateAccessor}.kt`.
  `bridges/test5.kt` and `bridges/test15.kt` leave the expected-failure ledger with it.

- **A `Unit`-typed local holds no machine value and is still readable.** `Unit` is a VALUE in
  Kotlin and the runtime owns the one instance of it, so a local of that type has nothing to put in
  a variable — the declaration is dropped and its initializer runs for its effect alone. That left
  every later mention of the local reporting a slot that was never declared, so
  `val u = println("x"); u.toString()` was declined where Kotlin answers `"kotlin.Unit"`. The slot
  is now remembered as a `Unit` one: a read produces no value, exactly as a `Unit`-returning call
  does, and a position wanting a reference gets the singleton from the same place every other
  `Unit` value comes from. An assignment to such a local is its right-hand side's effect and no
  more.

  `==` counts a `Unit` operand as a reference for the same reason: `println("x") == Unit` is true,
  and the ordinary reference path says so once the singleton is materialized.
  Tests: `tests/native_codegen_e2e.rs` (`a_unit_typed_local_is_still_a_value_that_can_be_read`);
  the corpus cases are `codegen/box/basics/{unit1,unit2,unchecked_cast10}.kt` and
  `codegen/box/controlStructures/kt237.kt`.

- **A `do`-`while` condition may read what its body declares.** Kotlin scopes a `do`-block's locals
  into the `while` that closes it, so the condition is the one place a loop test reads a local the
  BODY declares — `do { val limit = x + 5 } while (limit < 10)`. The test was lowered before the
  body whatever the loop's shape, so those reads found a slot that did not exist yet. The block the
  test fills is the same either way; only WHEN it is filled changes, and a post-test loop fills it
  after the body and the update. The loop frame is popped before that, for the same reason the
  pre-test condition is lowered before it is pushed: a jump written in a condition leaves the
  ENCLOSING loop, not this one.
  Tests: `tests/native_codegen_e2e.rs` (`a_do_while_condition_reads_what_its_body_declares`); the
  corpus case is `codegen/box/controlStructures/kt3280.kt`.

- **A `break` a diverging `finally` swallows does not leave its loop.** `while (true) { try
  { break } finally { return x } }`: the `finally` returns before the `break` arrives, so the break
  never completes and the loop is never left. The exit was marked reachable at the `break` site
  regardless of whether the jump was then emitted, which made the position after the loop live —
  and a function whose body is a `while (true)` nothing leaves may END there, that position being
  `Nothing`. Marking it reachable turned such a function into one that falls off its end. The mark
  now happens where the jump does.
  Tests: `tests/native_codegen_e2e.rs`
  (`a_break_a_diverging_finally_swallows_does_not_leave_the_loop`); the corpus cases are
  `codegen/box/finally/kt3894.kt`, `codegen/box/controlStructures/kt8148_break.kt` and
  `codegen/box/controlStructures/breakContinueInExpressions/breakFromOuter.kt`.

- **The end of a non-`Unit` function is the runtime's loud failure, not a decline.** Kotlin requires
  such a function to return on every path and the frontend has already checked it, so the one way a
  CHECKED program reaches the end is a `when` the frontend proved exhaustive whose subject matched
  no branch. `when (a) { A.V -> return "OK" }` over a one-constant enum is the shape: it needs no
  `else`, and this generator keeps the fall-through edge because proving exhaustiveness needs a
  hierarchy it does not have. kotlinc puts `NoWhenBranchMatchedException` at exactly that point for
  exactly that reason, and this now does the same — a few unreachable instructions, and never a
  wrong answer.
  Tests: `tests/native_codegen_e2e.rs`
  (`an_exhaustive_when_whose_arms_all_return_ends_the_function`); the corpus cases are
  `codegen/box/when/exhaustiveWhenReturn.kt`, `codegen/box/branching/when8.kt`,
  `codegen/box/regressions/kt18779.kt` and `codegen/box/when/enumOptimization/kt15806.kt`.

- **An `is` check asks a scalar operand through its box, on both backends.** Kotlin has no
  subtyping among the primitive types, so `n is Long` where `n` is an `Int` does not even compile
  and an `is` on a scalar reads as settled — but `5 is Number` and `1u is Comparable<UInt>` are
  true, and answering those needs the hierarchy. Each primitive's box carries the descriptor that
  has it, an unsigned one its own (which is what makes `(1u as Any) is Int` false), so boxing and
  asking is both correct and the only rule needed. `Unit` is the same question with the runtime's
  singleton as the operand.

  The native generator declined such a check outright. The JVM backend boxed correctly in its
  ordinary emit and NOT in the fused `instanceof; ifne` shape it uses for a condition, so
  `if (n is Number)` put an `int` where the verifier wants an object and the class was rejected
  with `VerifyError: Bad type on operand stack`. Both now box.
  Tests: `tests/native_codegen_e2e.rs` (`an_is_check_asks_a_scalar_operand_through_its_box`, which
  cross-checks the two backends and REQUIRES the native lowering); the corpus cases are
  `codegen/box/boxingOptimization/kt5844.kt`, `codegen/box/dataClasses/unitComponent.kt`,
  `codegen/box/inlineClasses/boxResultInlineClassOfConstructorCallGeneric.kt` and
  `codegen/box/primitiveTypes/kt36952_identityEqualsWithBooleanInLocalFunction.kt`.

- **A class may extend an exception the RUNTIME owns.** `kotlin.Throwable` and the exceptions
  Kotlin declares under it are classes no file declares, so the layout pass — which builds a class
  from its superclass's fields and vtable — declined every class extending one. It needs neither
  the source nor a classpath: the runtime already carries a `KType` and a storage layout for each,
  which is the same arrangement `kotlin.Enum` has had all along. The base's storage comes first
  (`Throwable`'s single `message`, traced by the collector like any other reference), its own
  `toString` fills `kotlin.Any`'s slot, and the subclass's descriptor points at the runtime's —
  which is the whole of what makes `catch (e: Exception)` take the subclass, since matching a
  clause walks exactly that chain.

  The base has no constructor to call: the object is already allocated, and what its constructor
  would have done is store what it was given, so the store happens where the call would have run.
  `Exception()` leaves the message null, which is Kotlin's null message; an argument shape the
  base's storage cannot hold is declined rather than silently dropped.

  Both spellings are one entry, because the two providers differ — a klib says
  `kotlin.IllegalStateException` and a JVM classpath says `java.lang.IllegalStateException`, and
  on the JVM the Kotlin name is a typealias for the Java one.
  Tests: `tests/native_codegen_e2e.rs` (`a_class_may_extend_an_exception_the_runtime_owns`, which
  cross-checks the two backends and REQUIRES the native lowering); the corpus cases are
  `codegen/box/exceptions/extend0.kt`, `codegen/box/classes/exceptionConstructor.kt`,
  `codegen/box/inference/tryCatchAtAssignment{,WithSmartCast}.kt`,
  `codegen/box/finally/someStuff.kt`,
  `codegen/box/controlStructures/tryCatchInExpressions/{multipleCatchBlocks,tryInsideTry}.kt` and
  `codegen/box/callableReference/adaptedReferences/manyDefaultsAndVararg.kt`.

- **An unsigned field hashes as the signed value it wraps.** Kotlin's unsigned integers are value
  classes, and `Ty::UInt` is a CONSTANT for `Obj(kotlin/UInt)` rather than a variant of its own —
  so the carrier reads one as the machine integer it wraps, while a `match` listing only the signed
  types misses it entirely. A value or data class holding one fell past every arm of the field hash
  and was declined as a type the generator did not recognize, though it had already agreed the
  value was a scalar. A constant pattern is invisible in exactly this way: nothing warns that the
  arm is absent.

  The answers are Kotlin's own, and a program can print a hash, so nothing here is free to choose
  them: `UByte` and `UShort` answer `data.toInt()` on the `Byte`/`Short` they wrap, which
  SIGN-extends — `255u.toUByte()` hashes to `-1`, not `255` — `UInt` answers its `Int` unchanged,
  and `ULong` folds its `Long` exactly as `Long` does. Equality needed nothing: it compares the
  carriers' bits, which is already right for an unsigned one.
  Tests: `tests/native_codegen_e2e.rs` (`an_unsigned_field_hashes_as_the_signed_value_it_wraps`,
  which cross-checks the two backends — the JVM lane runs against the real stdlib, so the
  sign-extension is the library's answer rather than this file's claim); the corpus cases are
  `codegen/box/inlineClasses/{kt27096,kt27132,kt34902,kt70461}.kt`.

- **A `Result` is its value, or a marker holding the exception.** `kotlin.Result` is a value class
  over `Any?` and the representation is Kotlin's own rather than a wrapper of this runtime's
  invention: a SUCCESS *is* the value, so `Result.success(x)` costs nothing and a `Result<T>`
  crosses a function boundary as an ordinary reference; a FAILURE is a marker carrying the
  exception. Every member is then one question about that single reference — whether it is the
  marker — and `isSuccess`, `isFailure`, `getOrNull`, `exceptionOrNull` and `getOrThrow` are each
  answered directly. A `null` success is representable and distinct from a failure, because the
  marker is never null.

  Two things the shape forced, both of which a wrapper would have hidden:

  - `Result.Companion` holds no state and every member of it is answered without reading a
    receiver, so it has no instance and needs none. One provider materializes that receiver before
    the call reaches the table saying so, and for such an object the honest thing to materialize is
    the null reference.
  - A member that answers a REFERENCE is not a member that answers what the call site asked for.
    `getOrThrow()` on a `Result<Int>` answers the box the `Result` holds while the site wants the
    integer, so the runtime's own answer type is declared and the conversion made — declaring the
    site's type instead read an `i64` return as an `i32`.
  Tests: `tests/native_codegen_e2e.rs` (`a_result_is_its_value_or_a_marker_holding_the_exception`,
  which cross-checks the two backends and REQUIRES the native lowering); the corpus cases are
  `codegen/box/inlineClasses/result/*.kt` and the `Result` cases under
  `codegen/box/inlineClasses/`.

- **A list answers its first and last element.** `first()` and `last()` with no predicate are a
  question about the ends of the list, which the runtime already held the pieces for. Kotlin raises
  `NoSuchElementException` on an empty one rather than answering null, with its own wording, and
  the distinction is not academic: a list of a nullable element type has a perfectly good null
  first element, and answering null for "empty" would make the two indistinguishable.

  The ONE-argument forms take a lambda and are a different question — an inline declaration whose
  body decides which element — so they are not these and still decline.
  Tests: `tests/native_codegen_e2e.rs` (`a_list_answers_its_first_and_last_element`, which
  cross-checks the two backends and REQUIRES the native lowering); the corpus cases are
  `codegen/box/boxingOptimization/kt6842.kt`, `codegen/box/callableReference/kt50172.kt` and the
  other `collections.first` cases.

- **An `indices` range is recognized where the program kept no name for it.** `x.indices` is
  realized as an `IntRange` and as nothing else, whatever the receiver is indexable as, but the
  READ carried no type. `b in a.indices` for a `b` that is not an `Int` is the ranges FACADE's
  `contains`, which reads its element from the RECEIVER — so a receiver with no type could not be
  recognized as a range at all, and the call fell through to the dependency-member path to be
  declined by name.

  The element arriving at its own width is the whole of that comparison's correctness: truncating
  a `Long` to the range's element makes `4294967296L` into `0` and answers `true`.
  Tests: `tests/native_range_contains_e2e.rs`
  (`an_indices_range_is_recognized_where_the_program_kept_no_name_for_it`); the corpus cases are
  `codegen/box/ranges/contains/generated/{array,charSequence,collection}Indices.kt`.

- **Preconditions raise Kotlin's own exception with Kotlin's own wording.** `require`, `check`,
  `requireNotNull`, `checkNotNull` and `error` are declared `inline`, so a provider holding their
  bodies splices them and nothing reaches a backend; a klib publishes no body to splice and the
  call arrives whole. Three facts are Kotlin's rather than a backend's, and a program can read all
  three:

  - The exception: `IllegalArgumentException` for the two `require` forms, `IllegalStateException`
    for `check`, `checkNotNull` and `error`.
  - The wording when the call writes no message: `"Failed requirement."`, `"Check failed."` and
    `"Required value was null."`.
  - `lazyMessage` runs ONLY when the check fails, and then once. It is not an optimization:
    `require(xs.isNotEmpty()) { xs.first().toString() }` is a program whose message throws when
    the check passes.

  So the shape is a branch around a raise rather than a call with operands, and the message is
  rendered with `toString()` — `require(false) { 42 }` carries `"42"`. The checked value is
  evaluated once, which `requireNotNull(f())` is entitled to.

  A message block that RETURNS from the enclosing function still declines. `lazyMessage` is a
  parameter of an `inline` declaration, so Kotlin lets the block written for it return from the
  caller; what arrives with no body to splice is an ordinary function value, and a non-local return
  through one is a miscompile rather than a slower answer. The decline comes from the block — the
  lowering finds no code behind the function value — which is why it is pinned by a test of its
  own in `tests/native_throws_e2e.rs`.
  Tests: `tests/native_preconditions_e2e.rs` (all five, each cross-checking the two backends and
  REQUIRING the native lowering); the corpus cases are `codegen/box/contracts/nonNullSmartCast.kt`,
  `codegen/box/delegatedProperty/provideDelegate/setValue.kt` and the other `kotlin.require` cases.

- **`buildString` and `buildList` make the subject the block fills.** Both are `inline`, so a
  provider holding their bodies splices them and nothing reaches a backend; a klib publishes none
  and the call arrives whole. The shape is the one the scope functions already get — evaluate,
  invoke, answer — with the subject MADE by the call rather than written by the caller.

  `buildString` answers the builder's `toString()`, which is a COPY, and that is observable: a
  program that keeps the builder through a capture and appends after the call still reads what the
  call answered. `buildList` answers the list it filled, because the read-only type it is declared
  with is a static claim and not a runtime one — the same position Kotlin's own takes. The
  capacity overload's argument is a hint no program can read back, and it is passed on.
  Tests: `tests/native_builder_scope_e2e.rs` (all three, each cross-checking the two backends and
  REQUIRING the native lowering); the corpus cases are
  `codegen/box/controlStructures/forIn*WithIndex*NameBasedDestructuring*.kt` and
  `codegen/box/inlineClasses/contextsAndAccessors/kt27513*.kt`.

- **`tailrec` promises the tail calls and nothing else.** A self-call with work after it is not a
  tail call, and Kotlin says so: it reports `NON_TAIL_RECURSIVE_CALL` and every Kotlin backend
  emits the call. So a `tailrec` whose body still holds one is an ordinary program, and a backend
  that declines it refuses a program kotlinc compiles — and compiles the same way.

  Common lowering had recorded two shapes as "a rewrite that did not finish", and neither is one:

  - A **surviving self-call** after the sweep. The sweep walks Kotlin's tail positions, so what it
    leaves behind is what Kotlin leaves behind. `diagnostics/functions/tailRecursion` is the corpus
    family written to exercise exactly this, at depths of 100 000 and 1 000 000: the tail call is
    looped and the `NON_TAIL_RECURSIVE_CALL` beside it recurses a handful of frames.
  - An **overridable member**. kotlinc refuses to loop that too, so leaving it recursive is
    agreement rather than a gap.

  What remains recorded is the one shape Kotlin loops and this lowering does not — a `tailrec` with
  context parameters — because there a backend emitting the recursion overflows where kotlinc's
  does not.
  Tests: `tests/native_tailrec_e2e.rs`
  (`a_tailrec_holding_a_non_tail_self_call_loops_the_one_that_is_a_tail_call`, which cross-checks
  the two backends); the corpus cases are all of
  `codegen/box/diagnostics/functions/tailRecursion/`.

- **A `for` over text reads each character at its index.** Common lowering turns `for (c in s)`
  into a counted walk writing two intrinsics — the length, and the read at an index. The length
  was answered and the read was not, so every `for` over a string declined on a loop nothing about
  the string runtime was missing.

  The index is a machine integer and the answer a `Char`; a loop variable declared `Char?` asks for
  the box, so the answer is converted to what the site asked for rather than handed straight back.
  Tests: `tests/native_strings_e2e.rs` (`a_for_loop_over_text_reads_each_character_at_its_index`,
  which cross-checks the two backends and REQUIRES the native lowering); the corpus cases are
  `codegen/box/strings/forInString.kt` and the other `StringGet` cases.

- **`kotlin.experimental`'s bit operations are the machine's, at the narrow width.** Kotlin gives
  `Int` and `Long` `and`/`or`/`xor`/`inv` as members and gives `Byte` and `Short` the same four as
  extensions in `kotlin.experimental`. That is where the library put the declaration, not a
  difference in what the operation means, so they are instructions here rather than a runtime call
  — the receiver never becomes an object to reach them.

  The width comes from the RESULT: `Byte.and(Byte)` answers a `Byte`, so the declaration already
  states the width all three operands share. It is observable — `0x0F.toByte().inv()` is `-16`,
  where the same operation at 32 bits would answer `-241` before narrowing.
  Tests: `tests/native_experimental_bitwise_e2e.rs`
  (`the_narrow_integers_answer_their_bit_operations_at_their_own_width`, which cross-checks the two
  backends and REQUIRES the native lowering); the corpus cases are `codegen/box/binaryOp/bitwiseOp*.kt`.

- **`mod` carries the divisor's sign, where `%` carries the dividend's.** The two disagree on
  exactly the operands whose signs differ, which is why Kotlin declares both: `(-7) % 3` is `-1`
  and `(-7).mod(3)` is `2`. The RESULT names the width the operands meet in, so `Int.mod(Long)` is
  a `Long` question; the narrow integers answer at their own width and are computed at `Int`, which
  is exact because the answer's magnitude is below the divisor's.

  On floating point the sign that decides is Kotlin's `sign`, not the sign bit: it answers NaN for
  NaN, and a NaN sign compares unequal to everything, which is what carries a NaN out of the
  adjustment instead of into an addition that would hide it. Either zero is answered as it stands,
  because `r != 0.0` is false for both. An integer zero divisor throws Kotlin's
  `ArithmeticException`, because `mod` is `%` adjusted and `%` throws.
  Tests: `tests/native_floor_mod_e2e.rs` (all three, each cross-checking the two backends and
  REQUIRING the native lowering); the corpus cases are `codegen/box/fp/remainderVsMod*.kt` and the
  `inlineArgsInPlace` pair.

- **A method reached through an interface that declares it at another representation is bridged.**
  An interface's member numbers are placed program-wide rather than in one class's table, so a
  representation change cannot be answered by replacing an entry in that table — there is no entry
  there. It is answered where the number is FILLED: the class records a bridge against the
  interface's key, and the interface region reads it before it reads the slot map, which is the
  arrangement a property accessor's interface bridge already used.

  Three things the shape forced, each of which pointing the number straight at the implementation
  would have hidden:

  - A number needing a bridge must not be SHARED with a spelling that does not.
    `interface Z1 : A<String>, B<String, Int>` overriding `foo(String, Int)` is callable through
    `A.foo(T, Int)`'s number as it stands and through `B.foo(T, U)`'s only after conversion, so
    `Z1`'s own spelling joins `A`'s number rather than `B`'s.
  - What an implementor inherits has to be chosen against ITS hierarchy. `Z1` and `Z2` above both
    fill `A`'s number, and one recorded answer meant the last interface laid out won — a class
    implementing the other answered with a body it does not have. The supplier is now the most
    derived interface in the implementor's own hierarchy that has one.
  - An interface's own bridge has no slot to forward through: the slot it would name is the
    interface's table index and not the implementor's. It calls the body outright, which is what a
    plain default already does, and is reached only where nothing overrides the member.

  Separately, and found by the same work: **a method spelled like a property's accessor is a
  method.** A property with a field is read through that field, so its accessor is synthesized; a
  same-named method is a declaration of its own. Keying it as the property's getter took it out of
  the method numbering entirely, so the base's slot kept the base's body and a call through the
  base jumped into whatever stood there. The name fallback now applies only to a property with no
  storage — an abstract `val` in an interface, which is what it was written for.
  Tests: `tests/native_interface_bridges_e2e.rs` (all three, each cross-checking the two backends
  and REQUIRING the native lowering); the corpus cases are all of `codegen/box/bridges/`, the
  `classDelegation` and `traits` delegation cases, and
  `codegen/box/extensionFunctions/*ExtensionSuper.kt` (KT-42176).

- **An array answers its elements as a list, and as a reversed one.** `xs.toList()` and
  `xs.reversed()` are a SNAPSHOT rather than a view, which is Kotlin's own answer and is
  observable: writing through the array afterwards leaves the list as it was, and
  `arrays/forInReversed/reversedOriginalUpdatedInLoopBody.kt` is written to check exactly that.

  The elements are boxed on the way in, because a list holds references and a primitive array does
  not — `IntArray.toList()` answering a `List<Int>` is that boxing. Which box each element gets is
  read from the ARRAY's descriptor, the only thing that knows how wide an element is and how to
  read its bits: a `LongArray` and a `DoubleArray` share a width and a trace bit and differ only
  there, and an unsigned array holds the signed one's bits with a different box, which is the
  difference between `4294967295` and `-1`.

  The unsigned arrays are declared in `kotlin.collections.unsigned`, a package of their own, and
  reach the same entry: the descriptor already says which they are.
  Tests: `tests/native_array_snapshots_e2e.rs`; the corpus cases are
  `codegen/box/arrays/forInReversed/*.kt`, `codegen/box/vararg/kt37715.kt` and the other
  `collections.toList` cases.

  **A JVM-lane defect this found, not fixed here:** `uintArrayOf(…).toList()` is rejected by
  krusty's JVM backend with `VerifyError: Type '[I' is not assignable to 'java/lang/Iterable'`. A
  `UIntArray` IS a `Collection<UInt>` in Kotlin, so `toList()` selects the `Iterable` extension;
  that backend hands the erased `int[]` to it without the value-class wrapper. The native test for
  the unsigned case is therefore native-only, with kotlinc's own answer as its expectation.

- **A property an interface declares and a class supplies with no override edge to name it.**
  Interface delegation is the shape: `class Q(a: A) : A by a` synthesizes `Q`'s own `x` and its
  accessors, and nothing records that they implement `A.x` — there is no source declaration to
  carry the edge, so the interface's number found no implementation and the file declined.

  Kotlin has already decided they implement it: a class does not compile with an interface property
  left unimplemented, and it cannot declare a second property of that name beside the inherited
  one. So an interface in the class's hierarchy declaring the same name IS the member those
  accessors fill, and matching by name is reading the language's rule rather than guessing — the
  same ground the inherited-slot match already stands on.

  Two more facts the missing edge had been hiding:

  - A CALL to such an accessor has to name the same key the table holds. An abstract `val` in an
    interface carries no accessor id, so its accessor reaches the method list as an ordinary method
    and is tied back by NAME — the layout read it that way and the call site did not, so every call
    to one asked for a `Function` key where the table held a `Getter`.
  - A class that supplies a property its SUPERCLASS already had takes that slot. The base's
    accessor may be synthesized from its field and so have no signature to match, so the inherited
    slot is found by the property's name. `class E : B(), C by D()` is that case, and kotlinc
    answers with the delegate (KT-70417); a private base property is excluded, because one is not
    inherited and a subclass may shadow it freely.
  Tests: `tests/native_delegated_properties_e2e.rs` (all three, each cross-checking the two
  backends and REQUIRING the native lowering); the corpus cases are `codegen/box/classDelegation/`,
  `codegen/box/classes/inheritance.kt` and the other "interface member with no implementation"
  cases.

- **An annotation class is a value, and its three `kotlin.Any` members are its members'.** Kotlin
  lets an annotation class be instantiated, and defines `equals`, `hashCode` and `toString` over
  the arguments rather than by identity. Construction and member reads needed nothing — the class
  lays out like any other — so what the declaration was waiting on was those three.

  An ARRAY member is compared, hashed and rendered by CONTENT. That is the whole difference from a
  data class, where an array member is compared by identity, which is why the two cannot share one
  synthesis. A floating-point member is compared through its BOX, so NaN equals itself and the two
  zeroes stay distinct.

  `hashCode` is a contract a program can read rather than an implementation detail: the sum of
  `(127 * name.hashCode()) xor value.hashCode()` over the members, which
  `annotations/instances/annotationEqHc.kt` computes in Kotlin and compares. The member name's hash
  is taken at RUN time so it is the same `String.hashCode` the program's own `name.hashCode()`
  reaches; folding it here would be a second statement of that function, to be kept equal by hand.

  The JVM backend's annotation IMPLEMENTATION class stays declined. It is that backend's own
  synthesis — a class implementing `java.lang.annotation.Annotation` — and reaches this model only
  by mistake.
  Tests: `tests/native_annotation_instances_e2e.rs` (all three, each cross-checking the two
  backends and REQUIRING the native lowering); the corpus cases are
  `codegen/box/annotations/instances/`.

- **A `vararg` constructor parameter is the array it already is.** A vararg parameter is
  PHYSICALLY an array and the IR records it as one, so a constructor taking it takes a reference
  like any other array parameter. The class model refused every such class by name, and there was
  nothing for the refusal to protect — the shape it was written against, a vararg argument
  recorded as its ELEMENT type, is not one this lowering produces.
  Tests: `tests/native_vararg_constructor_e2e.rs` (all three, each cross-checking the two backends
  and REQUIRING the native lowering); the corpus cases are
  `codegen/box/privateConstructors/withVarargs.kt`, `codegen/box/enum/varargParam.kt` and the
  other "vararg constructor parameter" cases.

- **An enum constant leaving a constructor argument out.** It is the same omission an ordinary
  construction makes, written a third way: `RED` where the enum's constructor declares
  `(val rgb: Int = 0)` is not an expression and not a supertype call, so neither of the collectors
  that find omissions saw it and every such enum declined.

  The constant reaches the class's own defaults wrapper — the one `Foo()` written as an expression
  reaches — which fills the frame in declaration order and runs the constructor, so a default that
  READS an earlier parameter finds it. What the entry supplies is the parameters it did not omit,
  at their own physical types.

  A constant with a BODY that also omits an argument still declines: its instance is a synthesized
  subclass whose own constructor takes the entry's arguments, while the defaults are recorded
  against the enum, so the wrapper has nothing to read for it.
  Tests: `tests/native_enum_defaults_e2e.rs`; the corpus cases are
  `codegen/box/defaultArguments/constructor/enum*.kt` and `codegen/box/enum/defaultCtor/`.

- **A secondary constructor the checker selected is reached as itself.** A `super(…)` delegation
  names the EXACT constructor the checker chose, and that may be a secondary one:
  `class E : A { constructor() : super() }` where `A`'s no-argument constructor is secondary.
  Taking the primary for every `super(…)` called it with the wrong arguments — Cranelift's own
  verifier caught it as a mismatched argument count, so it was a decline rather than a wrong
  answer, and it took every file holding such a constructor with it.

  An enum CONSTANT selects the same way: `ENTRY` on an enum whose primary takes a `String` and
  which also declares `constructor() : this("OK")` is a call to that secondary.

  A class HEADER selects the same way: `class D : A(4)` where `A`'s `(Int)` constructor is
  secondary is that selection written where a supertype is listed, and reading the primary's
  parameter list for every base call made the arity disagree.

  Kotlin admits no two constructors of one class with the same parameter list, so a secondary
  matching the selection IS the selection and the primary is what remains when none does.

  An INNER class's constructors all lead with the outer instance, and that PREFIX has to reach the
  right place. A `this(…)` delegation passes it on to the constructor it reaches — the call was one
  operand short of it. A `super(…)` one reaches no constructor of this class at all, so it stores
  the prefix itself, and the outer reference goes in BEFORE the base's constructor runs: that is
  Kotlin's own order and a base `init` calling an overridden method can observe it. A class with no
  primary constructor has only these, so leaving the store out left the field null and every read
  through it faulted.

  A base call that BOTH names a secondary and omits an argument still declines: the defaults
  wrapper fills the primary's frame, and a secondary's defaults are its own. So does a `super(…)`
  to a parent whose own constructor carries a prefix, which this constructor was never handed.
  Tests: `tests/native_secondary_constructors_e2e.rs` (both cross-checking the two backends and
  REQUIRING the native lowering); the corpus cases are `codegen/box/secondaryConstructors/`,
  `codegen/box/sealed/sealedInSameFile.kt` and `codegen/box/enum/emptyConstructor.kt`.

- **A value class wrapping a floating-point value answers its members.** The three synthesized
  members were declined on the premise that the runtime could not render the value. It can: a
  floating-point value has a box and its own `toString`, which is the shortest decimal that reads
  back as itself. `equals` and `hashCode` read the BITS, so NaN equals itself and the two zeroes
  stay distinct — the rule Kotlin's own `equals` states, and the opposite of what `==` on the
  machine answers.
  Tests: `tests/value_class_e2e.rs`
  (`a_value_class_wrapping_a_floating_point_value_answers_its_members`, which cross-checks the two
  backends and REQUIRES the native lowering).

- **A class that implements a function type is called like one.** `invoke` is the single dependency
  member this target already gives a FIXED slot: a function value's body sits right after
  `kotlin.Any`'s three, the runtime names that number itself (`KT_SLOT_INVOKE`), and every caller
  through a function type reads it. A class implementing `Function0<T>` puts its `invoke` there for
  the same reason a lambda does, and until it did, every such class was declined as an override of
  a dependency method.

  Only when every operand and the result are REFERENCES. A caller through the function type passes
  and reads references, and an `invoke(x: Int): Int` carries machine integers — that one needs a
  bridge and still declines rather than being pointed at.

  The slot sits AFTER `kotlin.Any`'s three rather than on top of one, so such a class keeps its own
  `equals`, `hashCode` and `toString`.
  Tests: `tests/native_function_type_classes_e2e.rs`; the corpus cases are
  `codegen/box/functions/invoke/*.kt` and `codegen/box/funInterface/`.

- **A class may override a member of a type declared outside the file.** The class model refused
  every such class by name — 94 methods and 41 properties across the corpus, the largest remaining
  decline. It need not: the override takes a slot of its own, like any member the class declares
  freshly. A caller naming the CLASS reaches it, and a caller naming the dependency type declines
  at the call site, where the type it named is still in sight.

  What the refusal was protecting is narrower than a whole class, and is kept:

  - `invoke` on a function type. Its number is FIXED — a function value's body sits right after
    `kotlin.Any`'s three and the runtime names that number itself — so a caller reads it rather
    than asking, and an override that cannot take that slot (its operands are not all references)
    would be reached there anyway. That one still declines.
  - The runtime's own answers for a dependency member. A receiver typed by a runtime-known type
    this file implements may be an object of the PROGRAM's, and every such table answers only for
    the objects the runtime MAKES — no static type tells the two apart, which is the whole reason
    those answers are the runtime's. So a receiver typed by a dependency this file implements
    declines by name, with the type it was asked of still in sight. For the collection dispatch,
    which of those receivers a declaration endangers is narrowed by the SHAPE rule in the next
    entry.

  What remains missing is fixed slot numbers for the members of runtime-known types, which is what
  would let a call THROUGH such a type dispatch; that phase is in `docs/IMPLEMENTATION_PLAN.md`.
  Tests: `tests/native_dependency_overrides_e2e.rs` and
  `tests/native_function_type_classes_e2e.rs`.

- **Which receivers a file's own collection class endangers is a question about the RECEIVER's
  shape, not about the file.** The collection guard above declined every collection member in any
  file that declared a class behind any collection type — one boolean for the whole file. That is
  wider than the hazard. A SHAPE groups the types whose objects are interchangeable at a call site:
  a set implementor answers a `Collection` and an `Iterable` receiver, so a list, a set, a range,
  an array and text are one shape; Kotlin's `Map` is no `Collection` and a `Sequence` is no
  `Iterable`, so each of those is its own, as is the iterator and the map entry. A class can only
  stand behind a receiver of a shape it implements something of, so the guard now asks whether the
  RECEIVER's shape is one the file implements, and a receiver of any other shape is answered as it
  would be in a file that declared nothing.

  The shapes are read from the same OVERRIDE edges the file-wide flag was, and no shape implies
  another — a class handing out an iterator of its own overrides `Iterator`'s members and is
  recorded there in its own right, while one returning a walk the runtime made is no hazard and
  records nothing. Inside a shape the guard stays wholesale, which is the point of a shape: no
  static type tells a program's `CharSequence` from a string, so a file declaring one still
  declines a list member.

  Together with the receiver dispatch for a nullary member of an implemented type, this is what
  makes a declared `Sequence` RUN: `class Counting<T>(source: Sequence<T>) : Sequence<T>` walks,
  because `iterator` is dispatched on the receiver and `listOf(…).asSequence()` beside it is of
  another shape entirely — the arm that falls through to the runtime, previously unreachable
  because the blanket guard declined its operand first.
  Tests: `tests/native_collection_shape_guard_e2e.rs` (an iterator, a map entry, and the
  deliberate coarseness inside one shape) and `tests/native_sequences_e2e.rs`
  (`::a_file_that_declares_its_own_sequence_walks_it`,
  `::a_declared_sequence_still_declines_the_members_it_endangers`).

- **The receiver dispatch for a member of a type this file implements now carries ARGUMENTS — and
  declines where Kotlin puts a SPECIAL BRIDGE in front of the override.** An argument adds one thing
  and only one: the two sides state the operand differently, so each is converted per arm — the
  implementor's own parameter type in its arm, the runtime entry point's carrier in the last — from
  a value evaluated ONCE, before the tests. Evaluating per arm would run a side effect twice. The
  entry point is chosen from the declaration's PARAMETER TYPES as well as the owner, which is what
  tells `s[i]` from a member of the same name over something else.

  What that alone got wrong is Kotlin's special-bridge rule, and the two-sided gate is what said so:
  `failed 4`, one of them a `SIGILL`. `Map<Any, Any>.get(key: Any)` is declared with a NON-NULL
  parameter, and a caller holding the same object as a `Map<Any?, Any?>` may pass `null`. Kotlin
  does not call the override there — it answers the member's default (`null` for `get` and `remove`,
  `false` for a `contains`, `-1` for an `indexOf`) because the argument cannot be what the
  declaration accepts. This dispatch has no bridge to put in front of an implementor's arm, so those
  members decline rather than reaching an override Kotlin would have skipped. Every one of them
  takes an argument, which is why the nullary members are untouched.
  `Comparable.compareTo` takes the same path, and is NAMED there rather than found: its answer is
  read from the receiver's DESCRIPTOR rather than from a table keyed on a shape, so there is one
  member and one entry point. A file that overrides it knows the classes that could be behind a
  `Comparable` receiver, so the call site tests for them and falls through to the descriptor — which
  is how a class of the program's and a boxed primitive reach one call site and each get their own
  order. A file that merely NAMES `Comparable` among its supertypes, or declares an enum, still
  declines: there is no override edge to read the implementors from.
  Tests: `tests/native_implemented_dependency_dispatch_e2e.rs`
  (`::a_member_with_arguments_dispatches_on_the_receiver_too`, whose index operand is counted so
  that evaluating it twice would fail, and `::a_member_with_a_special_bridge_declines`), and
  `tests/native_comparable_e2e.rs::a_declared_comparable_is_ordered_by_its_own_compare_to`.

- **A property reference to a `const val` of an object or companion reads the STATIC that holds
  it.** A reference's `get` reaches the property's storage, and for a class member that is a field
  of the receiver. A `const val` has no such field: Kotlin folds it at every use site and keeps the
  value in a static, so the reference answers that same value and reads it from there. Never
  mutable, so there is no write to reach; a site that somehow asked for one declines rather than
  writing a constant quietly.

  WHOSE static is not always the declaring object's. A COMPANION's `const val` lives on the OUTER
  class, which is where kotlinc puts it and what this layout follows, so both owners are admitted —
  under the property's own name, and only for a CONST: an ordinary property reached this way would
  be a guess about where its value is.

  Found by making the reference declines say WHICH construct they are. Every unrealized site
  reported `Checked(PropertyReference)`, the one thing they all have in common, which says nothing
  about which of them it is; they now name the shape — a dependency property, a member extension
  one, one with context parameters, or storage the generator did not find. That last one is what
  this entry closes, and the first three are what remains.
  Tests: `tests/native_const_reference_e2e.rs`. The reference compiler supplies those expectations
  directly rather than the two backends being cross-checked: reaching a `const val` through a
  reference or a delegate is a separate, pre-existing gap in the JVM backend, which emits a call to
  a getter the constant does not have. Corpus:
  `callableReference/callableReferenceOfCompanionConst.kt` and
  `delegatedProperty/delegateToConstVal.kt`.

- **An enum may leave an interface member to its ENTRIES.** The class model refuses a CONCRETE
  class that reaches an interface's abstract trap for an interface it implements: Kotlin would not
  have compiled such a class, so the implementation exists and the model failed to find it, and
  saying so at compile time beats a program that aborts where it should print an answer.

  An enum whose every entry has a BODY is the exception, and not by fiat: such a class is not
  instantiable as itself. Every instance is an entry subclass, the slot is filled there, and the
  same check runs for each of those — so a genuinely missing implementation is still caught, one
  level down. Kotlin marks such a class abstract for exactly this reason; common IR does not, so the
  shape is read from the entries. An entry WITHOUT a body is an instance of the enum class, and
  there the trap is reachable and the refusal stands.
  Tests: `tests/native_enum_entry_interface_e2e.rs` (the member reached through the interface, an
  enum splitting two interfaces between itself and its entries, and an enum with no entry bodies
  implementing its own); corpus:
  `enum/enumEntryReferenceFromInnerClassConstructor{1,2,3}.kt` and
  `callableReference/function/local/enumExtendsTrait.kt`.

- **`assert` is an intrinsic because its MODE decides before anything is evaluated.**
  `always-disable` evaluates NEITHER child, so a condition with a side effect does not have it, and
  a program can see that. Enabled, the condition is evaluated and branched on, and the failure side
  raises `AssertionError` — `"Assertion failed"` where no message was written.

  The message ARGUMENT is an ordinary argument and is evaluated with the others, whether or not the
  assertion holds; what `lazyMessage` makes lazy is the INVOCATION, which happens beside the
  failure and nowhere else. Building it on the failing side only is what this generator did first,
  and the two-sided gate caught it: `assert(c, xs.filter { … }::errorMessage)` must filter either
  way. The message crosses as the FUNCTION it is and the runtime invokes it, beside the failure it
  reports.

  That eagerness is NATIVE's, not the JVM's: there `assert` is an `inline` function whose whole
  body — the argument evaluation included — sits inside the `$assertionsDisabled` guard, which is
  why Kotlin's own test data marks the case `TARGET_BACKEND: NATIVE`. The `Runtime` mode is the one
  this target does not answer: whether assertions are on is a question about how the program was
  BUILT, and nothing in this generator can see that yet — answering it either way would be a guess
  a program can observe.
  Tests: `tests/native_assert_e2e.rs` (enabled, the eager-argument/lazy-invocation split, and
  disabled); corpus: `assert/{alwaysEnable,alwaysDisable,assertEnabledInConditionAndMessage,assertEnabledWithFunctionReference,assertDisabledWithFunctionReference}.kt`.

- **`kotlin.Comparator` is a functional interface the RUNTIME knows, so its conversion makes no
  object of its own.** A `fun interface` declared in this file becomes an object wearing that
  interface's table, because a caller reaches its member through a program-wide member number.
  `Comparator` needs none: nothing but its single `compare` is ever asked of it, and every caller is
  either the runtime or a call site that can see the type. So the conversion changes nothing — the
  object stays the ordinary FUNCTION VALUE the lambda already is, answering through the one invoke
  slot every function value declares, and `Comparator { a, b -> … }`, `Comparator(fn)` and a lambda
  passed where a `Comparator` is expected are all the same object.

  That is what lets the sort work with no calling convention of its own: `sortWith` invokes the
  comparator exactly as `map` invokes a transform. The sort is an INSERTION sort, which is stable,
  and stability is observable — Kotlin's `sortWith` promises it, so two elements the comparator
  calls equal keep the order they were in. It is quadratic, and a merge sort would need a scratch
  buffer nothing yet has a reason to allocate. `sortedWith` answers a NEW list and leaves its
  receiver alone, sorting while the list is still the growable shape a write goes through and
  freezing it afterwards.

  A program calling `compare` itself takes the same invoke, with the boxed answer unwrapped at the
  entry point rather than at the call site — the `Int` it asks about would only be unboxed again.
  Tests: `tests/native_comparator_e2e.rs` (a literal, a SAM constructor over a function value,
  `compare` through the type, and `sortedWith`'s stability); corpus:
  `sam/constructors/{comparator,nonLiteralComparator}.kt`,
  `callableReference/function/sortListOfStrings.kt`, `funInterface/kt49384.kt` and
  `funInterface/nonTrivialProjectionInSuperType.kt`.

- **A source class may extend `kotlin.Number`.** It is the second base the runtime owns, beside
  the `kotlin.Throwable` family and `kotlin.Enum`, and the simplest: it carries NO state — every
  member it declares is an abstract conversion — so a subclass of it is laid out exactly as a
  subclass of `kotlin.Any` is and contributes `Any`'s own three slots. The descriptor was there
  already: a boxed primitive points at it so `is Number` has something to compare, and a subclass of
  the program's now points at the same one, which is what makes `MyNumber(7) is Number` true and
  `MyNumber(7) is Int` false.

  Relaxing this alone was tried once and reverted, because `numberToDouble(FortyTwo)` reached the
  boxed-primitive intrinsic with an object that is not one and the program ABORTED. What closes that
  is the receiver dispatch for a nullary member of a type this file implements, which now reads the
  SCALAR table as well as the iteration one: `kotlin.Number`'s six conversions are there, each
  nullary and each answering at its own width, so a call through a `Number` receiver tests the
  object against each class of this file that could stand behind the type and falls through to the
  runtime — which is how a boxed primitive and a program's object reach the same call site and each
  get the right answer.
  Tests: `tests/native_number_subclass_e2e.rs` (through the class, the `is` questions, and the
  shared call site); corpus: `primitiveTypes/numberToChar/` (all six) and
  `primitiveTypes/virtualCallToCustomNumber.kt`.

- **A range whose bounds are ordered by `Comparable`, and the integral ranges reached through
  `ClosedRange<T>`.** Kotlin declares `rangeTo` on `Comparable<T>` and answers a
  `ComparableRange<T>`, seen through `ClosedRange<T>`: it keeps the two bounds as OBJECTS and asks
  each one how it compares. The runtime gets a shape for it beside the integral and floating ones —
  two references, no step and no walk, because `Comparable` names no successor — and every
  comparison goes through `kt_compare_any`, which reads the value's own descriptor. Its three
  inherited members are `ClosedRange`'s documented contract: two EMPTY ranges are equal whatever
  their bounds, an empty one hashes to `-1`, and `toString` is `"$start..$endInclusive"`.

  Whether the runtime may answer for one is a separate question, and it is the one
  `Comparable.compareTo` already asks: the order is read from a descriptor, so a class of the
  program's standing behind the element has none there. A file that declares its own `Comparable`
  therefore does not reach this shape at all — the construction is handed back to the general
  `rangeTo` path — rather than being ordered by a table that cannot order it.

  The same interface carries the INTEGRAL ranges: `fun f(r: ClosedRange<Int>)` names `ClosedRange`
  where the object is an `IntRange`, and the TYPE ARGUMENT is what says which element it is —
  `range_element` keys on the owner and the owner here names only the interface. A bound read
  through it is the erased `T`, so it goes back through the element's own width before it is boxed,
  which is what makes `(('a'..'c') as ClosedRange<Char>).start` read as the character it was built
  from. A type PARAMETER is deliberately not a reference element: the object behind a
  `ClosedRange<T>` in a generic body may be any of the three shapes, and only a concrete element
  rules the other two out.
  Tests: `tests/native_comparable_ranges_e2e.rs`; corpus: `ranges/contains/inExtensionRange.kt`,
  `::inRangeLiteralComposition.kt`, `::inOptimizableIntRange.kt`, `::inOptimizableLongRange.kt`.

- **`Delegates.notNull()` is one reference and the rule that reading it before writing is an
  error.** Kotlin's `NotNullVar` has no more content than that: `null` is not a value it can hold,
  its `T` being non-null by declaration, so the empty slot needs no flag beside it the way a lazy's
  `computed` does. The construction is a member of the `Delegates` OBJECT, which carries nothing, so
  it takes the path `Result.Companion.success` already takes — no receiver read, no operand passed.

  `getValue(thisRef, property)` drops `thisRef` as a lazy's does — the delegation already evaluated
  whatever it names, and this delegate reads none of it. The PROPERTY is read, but not as an object:
  the error names the property, as Kotlin's does, and the runtime cannot ask a property reference
  for its name (that is answered by a function of the emitted code's own, at no number the runtime
  knows). So the name is a string LITERAL the call site takes from the reference's declaration, and
  a `KProperty` this file did not build has no name to take — that call declines rather than
  reporting a wrong one.

  Two wrappers are looked through to find the declaration. An implicit COERCION, because the
  convention's parameter is `KProperty<*>` and the reference is narrower; and a read of a STATIC,
  because a top-level delegated property's `KProperty` is built once in the file's initializer and
  the call site reads it from there — so the reference is that static's initializer. One level of
  static only: a chain of them would be following assignments rather than reading a declaration.
  `Delegates.observable(initial) { property, old, new -> … }` is the second delegate, and the
  reason the two entry points dispatch on the OBJECT rather than being chosen at the call site:
  both are a `ReadWriteProperty<Any?, T>` there, and no static type separates them. Its callback
  runs AFTER the write, which is Kotlin's order — a callback reading the property sees the new
  value — and `observable`'s `beforeChange` is a constant `true`, so there is nothing to veto. The
  `KProperty` travels through as the OBJECT the delegation passed, never read, which is what lets
  this runtime hand a program its `name` with no reflection at all: it is the same object the
  emitted code built, answering out of its own table.

  `Delegates` itself joins the stateless runtime objects beside `Result.Companion`: it declares no
  state, and every member of it this runtime answers takes its arguments alone. That is needed as
  well as the member table, because `observable` is an `inline` declaration and its receiver is
  materialized before the table that says the receiver is not read.
  Tests: `tests/native_not_null_delegate_e2e.rs` (a member property, a local one read through a
  capture, a rewritten one, the `IllegalStateException` a read-before-write raises with Kotlin's
  own wording, and two observables — one reporting each write with the property's name, one
  notifying its instance); corpus:
  `delegatedProperty/{delegateWithPrivateSet,protectedVarWithPrivateSet,kt9712,observable}.kt`,
  `delegatedProperty/local/kt23117.kt` and
  `nameBasedDestructuring/{fullForm,shortForm}ExtraPropType.kt`.
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

- **A `super` call to a `suspend` member is refused by the CHECKER, at the `super` keyword.**
  Threading a continuation through a NON-VIRTUAL dispatch and resuming back into it is not modeled
  (the corpus's `coroutines/suspendFunctionAsCoroutine/superCall*`), and the project's rule for a
  construct it does not model is to decline the source. Emitting it anyway produced
  `invokespecial A.f:()Ljava/lang/String;` — the SOURCE descriptor, fixed when `module_calls`
  realized the super call — against a declaration that is
  `A.f:(Lkotlin/coroutines/Continuation;)Ljava/lang/Object;`, so the class could not link
  (`NoSuchMethodError: 'java.lang.String A.suspendHere()'`): an unlinkable artifact emitted with no
  diagnostic. The refusal belongs to the phase that SELECTED the target and has its suspend shape
  in hand. A backend guard has to rediscover that fact from a realization which no longer names it,
  and can then only recognize the call shapes reaching one particular node: the identical source
  with its superclass in a SIBLING FILE or in a DEPENDENCY has no same-file predeclaration to
  recover from, and an `@Outer`-labeled ENCLOSING dispatch is wrapped in a generated bridge that is
  not itself recorded as suspend — all three compiled and emitted the unlinkable call. One check,
  where `ResolvedSuperCall` is built, covers every spelling and every origin. A `super` call to an
  ORDINARY member and an ordinary virtual suspend call are both untouched: the rule keys on the
  TARGET being suspend, not on the dispatch being non-virtual. Test:
  `tests/suspend_super_call_refusal_e2e.rs`, which asserts the complete ordered ledger with
  positions for the direct, parameterized, typed, labeled-enclosing, sibling-file and dependency
  spellings, plus both negative controls.

- **A bridge method unboxes its RETURN value, and that adapter is not the ordinary unbox cast.** A
  bridge exists because a supertype's erased signature differs from the override's, and `emit_bridges`
  adapts both ends: it boxes a primitive argument, `checkcast`s a reference one, converts numeric
  widths, and boxes a primitive RESULT for a reference-returning supertype. The INVERSE of that last
  one was absent. When the delegated override hands back the erased generic REFERENCE
  (`getSize()Ljava/lang/Object;` for `var size: T`) while the supertype the bridge serves declares the
  PRIMITIVE (`interface C { var size: Int }`), the bridge pushed the reference and emitted `ireturn` —
  `VerifyError: Bad type on operand stack … Type 'java/lang/Object' is not assignable to integer`, an
  unverifiable artifact emitted with no diagnostic. (The setter direction was already right: an
  argument box was there from the start.) Measured from the reference compiler, the adapter is NOT
  `unbox_prim`'s: a NUMERIC goes through `java/lang/Number` — `checkcast java/lang/Number;
  Number.intValue()I`, and likewise `byteValue`/`shortValue`/`longValue`/`floatValue`/`doubleValue` —
  never through `java/lang/Integer`; `Boolean` and `Char` go through `java/lang/Boolean` and
  `java/lang/Character`; and the `checkcast` is OMITTED when the override's static return type already
  IS that owner (a `T : Number` base returns `()Ljava/lang/Number;` and kotlinc casts nothing, while a
  `T : Comparable<T>` bound keeps the cast). A bridge whose supertype declares a VALUE CLASS in its
  unboxed form takes the carrier out of that class's own `unbox-impl` (`checkcast IC;
  IC.unbox-impl()I`) — the class identity is unknowable from the bridge at emission time, because the
  value-class pass rewrites `erased_ret` to the carrier in the same step, so the JVM pass records it
  in `bridge_return_adaptations` — a backend-owned physical realization plan passed directly to
  bridge emission and keyed by the owning class and bridge ordinal. No classifier identity for a JVM
  boxing decision sits on common `Bridge` or `IrFile`. That holds for a
  REFERENCE carrier too (`checkcast Text; Text.unbox-impl()Ljava/lang/String;`): keying the adapter
  on the carrier alone sent a reference carrier down the ordinary `Object`-to-`String` narrowing,
  which never unboxed and handed the caller a `Text` where a `String` was declared. Nor may the two
  JVM types decide WHETHER to unbox: an `Any`-CARRIER value class (`@JvmInline value class
  Ref(val x: Any)`) has `Object` on both sides of the boundary, so a `concrete != erased` guard
  found them equal, emitted nothing and handed the caller the boxed `Ref` where the declaration says
  the carrier. The PLAN decides; the types only say what to write. And a NULLABLE value class whose
  carrier itself carries null (`Text?` over a non-null `String`) stays unboxed, so the delegated
  generic override may legally return `null` — `unbox-impl` is an instance call, and reaching it
  with null throws where the declaration says the bridge returns null. kotlinc branches around it
  (`checkcast Text; dup; ifnull → pop; aconst_null`, else `unbox-impl`) and so does krusty; the
  regression asserts both the instruction ledger and that the interface call really answers null.
  A BUILT-IN unsigned value class stays out of the global expression-rewrite map, but the dedicated
  callable-boundary value-class map retains its wrapper identity because boxed `kotlin.UInt` is not
  a `java.lang.Number`. That map therefore owns both its `unbox-impl` adapter and mangled bridge
  identity (`foo-pVg5ArA()I` for `UInt`). Tests exercise every unsigned bridge through its interface,
  in addition to comparing the instruction ledger.
  Test: `tests/bridge_return_unbox_e2e.rs`, an instruction ledger against the reference compiler for
  every signed primitive, both bounds, both value-class carriers and all four unsigned forms, plus
  runtime interface dispatch.

- **A `var` whose type is a BOUNDED type parameter emits an invalid `LineNumberTable` (open).**
  `open class P<T : Number> { var c: T? = null }` emits `setC` with a single line entry at
  `pc == code_length`, which the JVM rejects with `ClassFormatError: Invalid pc in LineNumberTable`.
  An UNBOUNDED `T` puts the same entry at pc 0, so the bound is what moves it. Found while fixture-
  reducing the bridge-return unbox above (whose test therefore holds its slot as `Any?`); it accounts
  for the corpus's `ClassFormatError:Invalid pc in LineNumberTable` bucket and is not fixed here.

- **A `try` and a `return` own their own `LineNumberTable` entries.** Four rules, each measured
  against the reference compiler and each previously absent, so a debugger stepping through a
  guarded region saw the finalizer's line where the source says otherwise:
  - A protected region OPENS on a `nop` carrying the `try` keyword's line, and the exception
    table's `from` is that `nop`. Starting the region on the body's first instruction shifted every
    offset in the method and lost the `try` line entirely — and, because a mark at an existing
    offset replaces the one already there, the body's own first mark overwrote it.
  - A `return` restores its own line at the PHYSICAL return instruction, after the parked value is
    reloaded (`iload_0` at one offset, `line 5` on the `ireturn` at the next) — not before the
    reload. A BARE `return` emits nothing of its own, so with a finalizer active its line would be
    claimed by the finalizer's first instruction; kotlinc anchors it on a `nop` ahead of the
    transfer and restores it again at the return. Both a value return and a void one therefore
    carry provenance, and the void arm simply had none.
  - The `goto` leaving an inlined `finally` on the normal path carries the `finally` block's
    CLOSING line: the jump belongs to the end of the finalizer, not to the statement after the
    `try`. Missing it costs two entries, not one — the finalizer's own line stays in effect into
    the catch-all handler, whose identical mark then deduplicates away.
  - The catch-all handler's entry — the `astore` parking the in-flight exception — belongs to the
    finalizer copy it introduces, so it opens on the finalizer's FIRST line rather than the
    `finally` keyword's.

  Tests: `tests/expression_line_marks_e2e.rs` (a bare return, a value return and an implicit `Unit`
  return each through a `finally`, plus an explicit return whose call is a constructor) and
  `tests/try_debug_lines_e2e.rs`.

- **A `try` with a `finally` reserves its two parked slots where it OPENS, and nested `try`s share
  them.** Such a `try` parks two things while a finalizer runs: the value a `return` out of it
  computed before leaving, and the exception its catch-all caught. kotlinc reserves both with the
  `try` itself, so every local an inlined copy of the finalizer declares sits ABOVE them; krusty
  allocated each where it was first used, which put the first copy's locals underneath and moved
  everything the `try` parks one slot up. The cost was not a name: a slot-higher parked exception
  is an extra `top` in every StackMapTable frame recorded while the finalizer runs, and a longer
  store in every copy, so the frames and the exception table's offsets both diverged.

  Nested `try`s SHARE both slots, which is also what kotlinc emits. Only one return is ever in
  flight, and a `try` inside the body runs its handler strictly before the enclosing one is
  entered — so the enclosing slots are free for it. The parked-exception slot stays in the reuse
  pool while the body is emitted and is taken back out before the handler, where it holds the
  exception across the whole inlined finalizer and a `try` inside that copy must not be given it.

  A TYPED catch's parameter takes the same slot the catch-all parks in, which is also what kotlinc
  emits: the two are never live at once — a catch body runs because its type MATCHED, and the
  catch-all parks only while unwinding past it — and the parked value is dead the moment the
  handler rethrows. A slot of its own pushed the parameter above the reserved one and cost a wide
  `astore` at every catch. A `return` written in a catch body is the other half of that scope: the
  slot the TRY reserved is live there, so it takes one of its own, as kotlinc's does.

  The LAST catch of a `try` with no `finally` falls through to the join instead of jumping to it:
  nothing stands between them, so the jump would be to the next instruction. Every other catch has
  the next handler, or its own copy of the finalizer, in the way and still needs it.

  Tests: `a_finally_with_its_own_handler_types_the_parked_exception`, which compares the complete
  exception table and the complete frame list — offsets, `top` padding and all — against kotlinc;
  `a_nested_finally_copy_stays_inside_the_outer_region`, which compares the complete code; and
  `a_typed_catch_does_not_guard_its_own_finalizer_copy`, whose complete table is the reference
  compiler's rather than a pinning of krusty's own.

- **A `catch` parameter is a debug local like any other.** It is DECLARED by its `IrCatch` rather
  than by a variable node, so it has no declaration expression the source-name and provenance
  tables can be keyed by; it carries the same two facts in the record that declares it
  (`IrCatchBinding`) and is rendered through the same JVM debug-name boundary as every other local.
  Writing the source spelling straight into the local variable table cost the `$iv` suffixes: the
  reference compiler names an inlined catch parameter `e$iv` at one expansion deep and `e$iv$iv` at
  two, exactly as it names an ordinary copied local, and krusty wrote a bare `e` at every depth. An
  expansion that clones a `try` nests the binding's provenance as it nests a local's.
  (`a_catch_parameter_is_named_where_it_is_declared`,
  `an_inlined_catch_parameter_is_named_at_its_expansion_depth`.)

- **A CALL's `LineNumberTable` entries: which physical operation is a dispatch, and which operands
  the call invented.** A multi-line call's operands each mark their own line as they are pushed, so
  by the time the `invoke*` is reached the line in effect is the last operand's. kotlinc puts the
  call's own line back at the dispatch. Getting this right is one question asked per physical
  operation, and every answer is pinned by a complete-table differential in
  `tests/expression_line_marks_e2e.rs` — the table from kotlinc and the table from krusty, offsets
  included, with kotlinc's own spelled out so a change in it is visible in the diff.
  - **A dispatch restores the call's line.** Every `IrExpr::Call` callee form, `IrExpr::New`,
    `IrExpr::MethodCall`, `IrExpr::InvokeFunction` (a function value's `FunctionN.invoke`),
    `IrExpr::EnumValueOf`, a property read or write realized as an ACCESSOR, and the intrinsics
    whose lowering IS a call — `PrimitiveCompare`'s `Integer.compare`, `String.get`'s `charAt`.
  - **An operation that dispatches nothing does not.** An array read or write, a field read or
    write, an arithmetic or comparison instruction: the operand's line stays in effect through it,
    and marking there would add an entry kotlinc does not have. So the rule cannot be "mark every
    intrinsic" — `x.compareTo(\n y\n)` and `a.get(\n i\n)` are the same source shape and take
    opposite answers. Both are asserted.
  - **Operands the CALL synthesized carry the call's line, not the last supplied argument's.**
    kotlinc returns to the call's line at the FIRST synthetic `$default` operand — the omitted-
    parameter placeholder — rather than at the `invokestatic`, because the placeholders, mask words
    and marker realize the ABI and not anything the source wrote. A supplied argument between two
    such runs puts its own line in effect and the next run restores the call's. With every
    parameter supplied, the mask push is that first synthetic operand. A defaulted CONSTRUCTOR
    takes the identical rule: it previously restored the line only at the `invokespecial`, three
    bytes late.
  - **Which operands those are is RECORDED, never recognized by shape.** A realized `Const(0)` and
    a source `0` are the same node, so the pass that invents them records their physical positions
    against the call (`jvm::default_call_operands::DefaultCallOperands`, the backend-owned table the
    default-call realization already fills). Nothing about JVM synthetic operands is persisted
    in common IR.

- **`enumValueOf<E>(name)` and `E.valueOf(name)` are different declarations, and are kept apart.**
  Both reach the same entry lookup and emit the same `invokestatic`, and kotlinc gives them
  opposite line tables: the classifier's own MEMBER is an ordinary dispatch, so the call's line
  returns at the invoke (`line 4: 6, line 3: 7`); the standard library's top-level `enumValueOf` is
  `inline`, so what follows is its expansion and kotlinc marks the call SITE, keeping the call's
  line and its argument's line as TWO entries at one offset (`line 3: 6, line 4: 6`). Checking
  collapsed both onto one `FirClassifierCallable::EnumValueOf`, so krusty could only pick one
  answer; `TopLevelEnumValueOf` and `IrExpr::EnumValueOf::declaration` now carry the selection to
  the backend. Two entries at one offset needed a debug-line operation of its own
  (`CodeBuilder::mark_line_retained`): ordinary marking replaces at a repeated offset, which is
  correct everywhere else and here would lose one of the two positions. The same operation opens an
  emitted `inline fun` TEMPLATE on its own body line, which kotlinc records beside the body's first
  instruction line. Tests: `a_multi_line_enum_value_of_keeps_both_entries_at_its_dispatch`,
  `a_multi_line_enum_member_value_of_returns_to_its_line`,
  `a_reified_enum_value_of_template_marks_its_call_site`.

- **An assignment's write dispatches where its LVALUE is named.** `b\n    .value =\n    x` puts the
  setter call on the `.value` line, the way a multi-line call's dispatch returns to its selector's
  line — kotlinc records `line 4: 6, line 6: 7, line 5: 8, line 7: 11`, and krusty had no entry for
  the accessor at all. A member assignment is a STATEMENT, whose only line was its first, so the
  lvalue's line is now carried from the parser (`assignment_target_lines`, parallel to the existing
  `assignment_target_spans`) through `FirStatementDebugLines::target` to the lowered write. Test:
  `a_multi_line_property_write_returns_to_its_accessor_line`, with
  `a_multi_line_property_read_returns_to_its_accessor_line` for the read.

- **A labeled `break`/`continue` leaves the loop it NAMES, or the file is refused.** The emitter
  looked the label up on its loop stack and fell back to the innermost loop when it found nothing,
  so a transfer and its loop that disagreed about which loop this is produced a jump the source
  never wrote — silently. There is no fallback now: an unmatched label is an emit error and the
  file is skipped, which is the same fail-closed answer the operand-arity contract gives. Tests:
  `break_continue_e2e::labeled_break_and_continue_leave_the_loop_they_name` (a labeled `break` and
  a labeled `continue` out of a nested loop, each through a `finally` that must run on the way),
  and `jvm::ir_emit::try_emission::tests::a_break_naming_a_loop_that_is_not_open_is_refused_not_redirected`.

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

- **An inline expansion's parameters are locals OF THAT EXPANSION, named by their ROLE.** A spilled
  local's debug name is what a debugger shows while stepping through an inlined body, and krusty
  produced the wrong one in three distinct ways:
  - An argument that was already a local read reused the CALLER's slot, so the inline parameter had
    no identity of its own and vanished from the spill names. kotlinc copies it; the expansion's
    parameter gets its own slot, name and lifetime. A FUNCTION-typed argument is the exception and
    keeps the caller's slot: an inline function parameter is SPLICED at each of its call sites
    rather than stored, so there is no local to name. Copying one hid the lambda from the splicer,
    so a forwarded `p` — `inline fun block(p: () -> Unit) { blockImpl(p) }` — materialized a
    `Function0` whose implementation method was never emitted and the program died at its first
    call with `NoSuchMethodError` (box `labels/nestedInlineLabels.kt`, reduced into
    `a_forwarded_inline_lambda_parameter_is_still_spliced`).
  - A member inline EXTENSION binds two receivers at once, and Kotlin keeps them distinct: the
    containing class's `this` and the receiver being extended are different values. One role could
    not stand for both, so `IrInlineLocalRole` has `DispatchReceiver` and `ExtensionReceiver`, and
    the JVM boundary owns their spellings — `this_` for the callable's own `this`,
    `$this$<callable>` for the receiver it extends, each with one `$iv` per inline depth.
  - WHERE the extension receiver sits is a semantic coordinate, not position zero: Kotlin signs a
    context extension `(contexts…, receiver, values…)`, so it follows the context parameters the
    callable declares.
  - A spliced lambda's own VALUE parameters are locals of the splice and keep their source names;
    its CAPTURES are not — they are the enclosing locals, already named where they were declared.

  - WHICH function-typed parameters splice is a MODIFIER, not a type shape. `noinline` marks the
    one function-typed parameter whose argument is a real closure: it owns a local, a name and a
    lifetime of its own, exactly like a value parameter, and forwarding it into a second expansion
    copies it rather than handing on the caller's slot. `crossinline` is NOT this — it only forbids
    a non-local return from the lambda, and the reference compiler still inlines the body — so a
    `crossinline` parameter owns no local either. Both are function-typed exactly like the spliced
    parameter beside them, so the role is carried from the declaration that wrote it: the parser
    records it on the value parameter, the compact header publishes it, and the expansion reads the
    callee's published parameter rather than inspecting the argument's type. A parameter whose role
    was never published declines the expansion instead of guessing, because either guess silently
    erases something — the parameter's identity, or the splice.
    (`a_noinline_parameter_keeps_its_own_local_where_a_spliced_one_has_none`.)

  Nothing is recovered from a name here: the role and the coordinate are recorded where the
  expansion is built, and the `$this$`/`$iv` spellings exist only at the JVM boundary. Tests:
  `an_inline_expansions_parameters_and_receiver_are_named_spills`,
  `a_member_inline_extension_names_both_of_its_receivers`,
  `a_context_parameter_does_not_displace_the_inline_receiver`,
  `a_spliced_lambdas_value_parameters_keep_their_names`,
  `a_spliced_lambda_names_each_of_its_value_parameters`,
  `a_lambda_declared_inside_an_inline_function_gains_its_frame`,
  `a_noinline_parameter_keeps_its_own_local_where_a_spliced_one_has_none`, and the naming unit tests
  in `jvm::debug_local_names`.

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

- **A TAIL-ONLY inline expansion produces its value; it does not loop to carry one out.** An
  expansion of a non-`Unit` inline function lowers to `var result = zero; loop@ while (true) { body;
  break@loop }; result`, because a non-local `return` from the middle of the body has to carry a
  value out past everything after it. When the only `return` IS the body's tail, none of that is
  needed: the value is simply the body's, which is what kotlinc emits — it leaves it on the operand
  stack. The loop form costs an unnamed local, and when the expansion crosses a suspension that
  local takes a continuation field kotlinc has no counterpart for, so the spill arrays diverged.
  - The shape is PROVED before anything changes, and proving it cannot change anything: the chain
    of statement blocks from the expansion's root down to the rewritten return is collected from a
    SHARED reference, and only then is the `Unit` placeholder allocated and the blocks rewritten.
    Allocating first left an orphan `UnitInstance` in the arena whenever the answer turned out to
    be no — the block shapes were restored, the allocation was not, and a refused optimization
    still shifted every expression identity after it.
  - A statement-bodied inline function wraps its body one level deeper, so the promotion descends,
    and EVERY block on that path becomes value-producing; one left ending in a statement discards
    the value.
  - Refusal is the common case and must be total: a statement after the return, a refusal one level
    down, and a block that already produces a value each keep the loop form with the arena
    untouched.

  Tests: `fir_lower::inlining::tail_promotion_tests` — seven, including
  `a_refused_unit_return_allocates_nothing`, which compares the arena's LENGTH as well as its nodes
  and is the one that fails if the allocation moves back ahead of the proof — and
  `tests/inline_tail_expansion_shape_e2e.rs`, which reads the lowered IR: a sole tail return expands
  with no exit loop, an early return keeps one, a non-tail `Unit` return keeps one, and a `Unit` tail
  return is compiled and RUN to show its returned expression is evaluated exactly once.

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

- **An enum EMITS its secondary constructors, with the synthetic prefix forwarded.** The enum
  writer is a separate path from the ordinary class writer and emitted none of them, so
  `enum class My(val s: String) { ENTRY; constructor(): this("OK") }` produced a class whose
  `ENTRY` called an `<init>` declared nowhere — `NoSuchMethodError: My: method
  'void <init>(java.lang.String, int)' not found`, an artifact that could not link, emitted without
  a diagnostic. Three facts, each measured against the reference compiler:
  - Every constructor of a Kotlin enum carries the synthetic `(String name, int ordinal)` ahead of
    what the declaration wrote. Those slots are forwarded verbatim to a `this(…)` delegation and
    spliced into the target's descriptor, and they are NOT value parameters, so the body's value
    ids still start at the first declared one — the same split the primary already made, now
    shared through `SecondaryConstructorEmitter`'s `owner_prefix`. The emitted body is
    byte-identical to kotlinc's (`aload_0; aload_1; iload_2; ldc "OK"; invokespecial
    <init>:(Ljava/lang/String;ILjava/lang/String;)V`).
  - An enum's constructors are PRIVATE, secondary ones included. Emitting one public would expose
    a way to construct an enum instance the source never granted.
  - The prefix is part of the constructor's PHYSICAL PARAMETER DESCRIPTION, not a detail of the
    descriptor. `method_parameters::OwnerConstructorPrefix` carries the types and the reflected
    identities together, so `-java-parameters` describes an enum secondary as kotlinc does —
    `$enum$name` and `$enum$ordinal`, both `ACC_SYNTHETIC`, then the source parameters — and the
    default stub carries it too: `constructor(k: Int = 5)` on `enum class My(val s: String, val n:
    Int)` is `(Ljava/lang/String;IIILkotlin/jvm/internal/DefaultConstructorMarker;)V`, the owner
    prefix, the declared parameters, the mask, the marker. Adding the prefix at the emitter's call
    site alone left `method_parameters::secondary_constructor` asserting on an arity two short and
    the stub emitting an overload every entry that omits an argument calls and no declaration
    provides.
  - A secondary constructor records its SOURCE shape in a generic `Signature` whenever that differs
    from its descriptor — kotlinc's own rule for the attribute. It is formatted from the SEMANTIC
    parameter types, never by concatenating descriptors or retrying formatter failure with erased
    JVM types: a `Signature` exists precisely to say what a descriptor cannot, so the fixture-owned
    `constructor(values: Envelope<String>)` signs `(LEnvelope<Ljava/lang/String;>;)V`, not
    `(LEnvelope;)V`. An owner prefix makes the descriptor differ by itself (`()V` for an enum's
    `constructor()`), so every enum secondary carries one; without it reflection reports the ABI
    prefix as if the source had declared it, and two constructors differing only by the prefix
    become indistinguishable.
  - The synthetic default overload takes the CONSTRUCTOR's own access, not a fixed
    `PUBLIC|SYNTHETIC`. An enum's constructors are private, and kotlinc marks their overload
    `ACC_SYNTHETIC` alone (`0x1000`); publishing it public would grant a way to build the class
    that the declaration does not.
  - The declared access is the CONSTRUCTOR's own visibility. A `private constructor` is
    `ACC_PRIVATE`, a `protected` one `ACC_PROTECTED`; a secondary constructor's modifiers used to be
    dropped by the parser outright, which published every one of them as `public`. Sealed, value-
    class-parametered and enum constructors stay private regardless, for the reasons above.
  - A secondary constructor's `LineNumberTable` is built from lines its own DECLARATION owns, each
    recorded where the syntax was live and carried to the constructor on
    `IrSecondaryCtor::lines`: the `constructor` keyword, each parameter's default expression, the
    `this`/`super` keyword, and the declaration's closing line. They are four different source facts
    and can be four different lines, so none may stand in for another — the stub used to take "the
    declaration" from the first default expression, then the delegation, then the PRIMARY's
    class/field/closing-paren provenance, which attributed the secondary's code to another
    declaration entirely. kotlinc enters the synthetic overload on the `constructor` keyword, fills
    each masked parameter on that parameter's default, returns to the keyword for the branch, and
    delegates on the declaration's closing line; krusty's table is identical, pinned by a multiline
    ledger in `tests/enum_secondary_constructor_e2e.rs` whose three facts are on three lines.
  - Still open: a NON-private secondary constructor's own single entry sits at pc 0 where kotlinc
    puts it at pc 6. kotlinc enters such a constructor through an `Intrinsics.checkNotNullParameter`
    guard per non-null reference parameter; krusty emits those only for PRIMARY constructor
    parameters. The line is the same on both sides — only the prologue it follows differs — and the
    synthetic overload, which has no such prologue, matches exactly. Pinned to that exact size by
    `a_non_private_secondary_constructor_differs_only_by_its_missing_null_check`.
  - Still open: the declared constructor's table is one entry even when its delegation spans lines,
    where kotlinc marks each argument's own line and returns to the delegation's. That is
    expression-line provenance for a constructor body, the same boundary as an ordinary call's
    dispatch line, not a declaration fact.
  - An enum declaring ONLY secondary constructors has no primary to emit: every entry names one of
    the secondaries, and registering the synthesized primary anyway collided with a no-argument
    secondary — both are `(String, int)V` — failing to load with `ClassFormatError: Duplicate
    method name "<init>"`. Its bytes are still built so the constant pool interns in kotlinc's
    order.
  - A body-only enum secondary has an `ImplicitEnumBase` delegation in common IR. It is not dropped
    merely because the source wrote no `this(…)` call: the JVM backend supplies `java/lang/Enum` and
    forwards the backend-owned name/ordinal prefix, while property/init initialization runs in this
    direct-base constructor before its body. Those physical prefix slots remain typed across frames
    recorded by branchy delegation arguments.
  - A bodied entry is a separate subclass. Krusty does not emit nestmate attributes yet, so an enum
    secondary selected by such an entry uses the same package-private synthetic accessibility
    bridge as a selected primary constructor; leaving the source constructor physically private
    makes the subclass fail with `IllegalAccessError`.

  Still failing, recorded rather than guessed at: an enum with ZERO entries loses every synthesized
  member because the JVM IR carries no `is_enum` flag — enum-ness is read as
  `!enum_entries.is_empty()` (`emptyEnumValuesValueOf.kt`). Test:
  `tests/enum_secondary_constructor_e2e.rs` and the enum fixture in
  `tests/java_parameters_attribute_e2e.rs`.

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

- **A super-constructor argument's captures come from the constructor's synthetic prefix
  parameters, however deeply the argument nests them.** A local class lifts each captured local
  into a constructor prefix parameter and stores it into a field, but the super-constructor call
  runs BEFORE that store — before the instance exists at all. Reading such a capture directly was
  already routed to the prefix parameter (`ConstructorCaptureRead`); handing it to an anonymous
  object nested inside the argument was not, and the nested object's capture was materialized as
  `ClassStorage` — `getfield` on `uninitializedThis`, which the JVM verifier rejects outright
  (`Type uninitializedThis is not assignable to 'box$Local'`) and which a target without a verifier
  would answer with a zero. The checked capture source now carries `ConstructorCapture`, the
  passed-on counterpart of `ConstructorCaptureRead`, chosen by the same checked predicate
  (`reads_constructor_prefix_capture`) that the direct read uses, so one rule covers both. Bytes
  for every other shape are unchanged. Tests:
  `tests/anon_object_capture_e2e.rs::anonymous_object_in_a_super_constructor_argument_reads_the_constructor_capture`
  and `::nested_anonymous_objects_in_a_super_constructor_argument_read_the_constructor_capture`
  (the second pins the transitive case: an anonymous object inside an anonymous object inside the
  argument). Corpus: `closures/captureInSuperConstructorCall/localCapturedInAnonymousObjectInLocalClass.kt`
  and `…2.kt`.
- **A checked `Nothing` value carries its completion contract into common IR.** Non-null `Nothing`
  is semantically divergent, but a target can still have to realize a physical fallthrough path.
  On the JVM, a declared `Nothing` call has a `java.lang.Void` result slot, while `null!!` retains
  its asserted reference after `Intrinsics.checkNotNull`; both values must be discarded before the
  path ends with `KotlinNothingValueException`. Without that completion, a primitive sibling such
  as `if (c) true else null!!` reaches the merge with a reference where the verifier requires an
  integer. Both checked-FIR lowering and the legacy AST bridge attach the same `BottomValue`
  contract. Common IR records only semantic completion; the JVM emitter derives the physical stack
  effect from the actual invocation/assertion it emits, then discards that result mechanically.
  Tests: `tests/nothing_call_branch_e2e.rs` covers Boolean, every primitive width, `when`, the
  reference control, exact kotlinc bytecode parity, and declared/inferred external calls. Unit
  contracts beside both lowerers verify that neither pipeline can omit the common-IR marker.
- **`++p` on a property reads it twice; `p++` reads it once — in statement position too.** Kotlin
  defines `++p` as `p = p.inc()` followed by the VALUE of `p`, which for a property is a fresh read
  through its getter, while `p++` binds the old value to a temporary. For a custom getter the
  difference is observable as a call count, and kotlinc emits the prefix re-read even where the
  value is discarded. krusty's value position already had the rule; statement position dropped it on
  the reasoning that a discarded value leaves no prefix/postfix distinction to preserve — but the
  distinction is not in the value, it is in the number of accessor calls. The AST had kept `prefix`
  on `Stmt::IncDec` for exactly this and nothing downstream read it.
  The re-read is the SAME selected access as the first read: same getter, receivers, context
  arguments, and declaration substitutions. Spelling it a second time invites leaving part of the
  selection off, which is what happened — the second read of a context-parameter property was built
  with an empty context-argument list, so its getter call came out one operand short and the backend
  bailed. Both reads (and the pre-existing value-position pair, which had the same hole) now come
  from one builder, `selected_property_read`.
  Tests: `tests/property_accessor_increment_e2e.rs::a_prefix_increment_reads_the_property_again_and_a_postfix_one_does_not`
  and `::a_prefix_increment_of_a_context_property_re_reads_with_its_context_argument`, both run under
  the provisioned reference compiler as well. Corpus:
  `intrinsics/prefixIncDec.kt`, `statics/incInObject.kt`, `statics/incInClassObject.kt`.
- **A shared-cell capture stays one cell however many callables it crosses, and is forwarded BY the
  cell.** `var ok = "fail"; fun mk(): C = object : C { override fun i() = ok }` — the anonymous
  object must see the write `ok = "OK"` that happens after `mk()` returns, so it has to hold the
  same `Ref` the enclosing function holds. Two independent halves both got this wrong for an
  anonymous object nested one callable deep (a local function, a lambda, or a local class), and a
  lambda in the same position was already right:
  - The checker decided `shared_cell` from the reassignment sets of the body being checked. Inside
    a nested callable those sets are empty — the write lives in an enclosing frame — so a capture
    whose source binding was ALREADY a shared cell was published as a plain value. A binding now
    carries `shared_storage_cell`, set where a local classifier's capture fields are declared, and
    a capture of such a binding stays shared without re-deriving the write.
  - Lowering forwarded the capture with `captured_value`, which UNWRAPS the cell. That is right for
    a read and wrong for a hand-off: it feeds the generated class's `Ref`-typed field a bare
    element. The `Captured` capture source now uses `captured_value_holder` when the capture is a
    shared cell.
  The rule and the write analysis it rests on live in one place, `src/resolve/capture_storage.rs`:
  four named facts about the binding (delegated, mutable, already a cell, written here) and the AST
  walk that answers the last one. Named rather than positional, because three of the four are
  booleans and swapping two of them is a silent miscompile rather than a type error.
  Either half alone still fails, in opposite directions — `Ref` into a `String` parameter, or
  `String` into a `Ref` parameter — and the JVM verifier names both. Tests:
  `tests/anon_object_capture_e2e.rs::an_anonymous_object_in_a_local_function_forwards_the_shared_cell`,
  `::an_anonymous_object_in_a_lambda_forwards_the_shared_cell`,
  `::an_anonymous_object_in_a_local_class_forwards_the_shared_cell`, and
  `::an_anonymous_object_two_callables_deep_writes_through_the_shared_cell` (the write direction).

- **A lambda written in a super-constructor argument takes the capture as a PARAMETER, not off
  `this`.** `class Local(k: String) : Base({ o + k })` — the lambda's capture values are bound at
  the construction site, inside the constructor prefix, where the instance does not exist. krusty
  checked the lambda body with the prefix rule cleared, so `o` came back as a class-storage read
  through a captured implicit receiver: the lambda captured the uninitialized `this` and read the
  field off it (`VerifyError: Type uninitializedThis is not assignable to 'box$Local'`). The prefix
  rule now follows the lambda in — and only a lambda: every other nested body (a local function, a
  local class, an anonymous object's own members) clears it, because those do not run inside the
  prefix. The capture it needs is carried in as one more capture parameter, so `FirCapture::source`
  became [`FirCaptureSource`]: a value slot of the enclosing body, or the enclosing constructor's
  synthetic prefix parameter. Nothing else about captures changed — the same forwarding, merging and
  shared-cell upgrade machinery carries the new source outward through nested lambdas unchanged.
  The same rule extends to the enclosing-INSTANCE capture of a local or anonymous class declared in
  the argument, which had the identical defect one level up.
  Which of the two the read means is the CHECKER's answer and travels on the node
  ([`FirConstructorCaptureSite`]): in the constructor body the synthetic prefix parameter is in
  scope and is read directly, and inside a lambda there the value is that frame's capture, at the
  depth it was registered under. The depth grows with the nesting — 0 one lambda in, 1 two lambdas
  in — so it is not a constant a lookup may leave out, and lowering performs exactly the one lookup
  the coordinate names: a coordinate that names no slot fails rather than falling back to reading
  the constructor's parameter, which in a body that is not that constructor is another
  declaration's value of the same type.
  Tests: `tests/super_argument_capture_e2e.rs` (a captured local, an enclosing property, a lambda
  inside a lambda, two lambdas sharing one capture, a write through a shared cell, an enclosing
  instance reached from a nested anonymous object, and a call to an enclosing local function),
  `fir::body_check::local_class_tests::a_constructor_prefix_read_carries_its_frame_in_fir` and
  `fir::body_check::driver_tests::nested_anonymous_super_argument_reads_its_enclosing_instance_from_the_prefix`
  for the checked shape, and
  `fir_lower::tests::a_capture_coordinate_naming_no_slot_fails_rather_than_reading_the_parameter`
  for the lowering contract. Corpus: eight of the eleven red cases under
  `closures/captureInSuperConstructorCall`, plus `super/kt4173_2.kt`.
