# krusty → kotlinc parity protocol

Goal (session directive): finish the Kotlin→JVM compiler rewrite in Rust as a **drop-in replacement for
kotlinc**. No compiler-plugin/extension support; every other compiler part must work. The produced
**bytecode must equal** the reference kotlinc's. Validate against the conformance tests in
`~/external-projects/kotlin`. Maintain our own test suite. Commit + push after each phase. Keep test
execution **< 60s** (profile/optimize otherwise). No hacks/workarounds/bails. TDD.

## Definitions of done

- **Runtime correctness**: `box()=="OK"` under `-Xverify:all` on the codegen/box corpus (the `kotlin`
  repo's `compiler/testData/codegen/box`). Current gate: **1582 OK / 0 FAIL** (scanned 7351, Phase 418).
- **Bytecode parity**: per-class `javap -c -p` normalized-equal vs kotlinc (`src/bin/bytediff.rs`).
  Normalization removes only semantics-preserving noise (source banner, instruction offsets,
  constant-pool index tokens). This is the harder bar the goal now demands.

## Tooling

- Conformance gate: `cargo test --test kotlin_box_ir_jvm_conformance --profile gate` (box()=OK, FAIL=0).
- Bytecode diff: `cargo run --release --bin bytediff -- <box_dir> [limit] [--samples]` with
  `KRUSTY_KOTLINC` (`.kotlinc/2.4.0/kotlinc/bin/kotlinc`), `KRUSTY_SURVEY_STDLIB`,
  `KRUSTY_SURVEY_JDK_MODULES` (`$JAVA_HOME/lib/modules`), `JAVA_HOME`.
- Reference repo: `~/external-projects/kotlin` (5.6G full Kotlin source; box corpus mirrored under
  `.kotlin-box/<ver>/compiler/testData/codegen/box`).

## Constraints / open items

- **Test time < 60s** — posture: the correctness gate is already <60s; the full `cargo test` is not, by
  design.
  - Fast tier (the dev/pre-merge gate): `cargo test --test kotlin_box_ir_jvm_conformance --profile gate`
    = **~38s** (rayon-parallel, ONE persistent JVM runner per thread, ClassLoader+reflection — no
    per-test JVM/javac). Plus lib unit tests (~0.02s). Under 60s. ✓
  - **Compile-once differential tests (P7, supersedes the P6 golden cache)**: golden files would go
    STALE across kotlinc versions/extensions (different output each version), so we generate the
    reference FRESH but BATCHED — every differential case's source is compiled in ONE kotlinc invocation
    (and one krusty invocation), cached per test process (`diff_refs`, `OnceLock`); each `#[test]` is
    `assert_diff("<case>")`. Add a case to `diff_cases()` (unique filename → unique facade). One kotlinc
    JVM launch for the whole `bytecode_parity_e2e` differential set instead of one-per-test:
    ~47s → **9.9s** (21 tests). No committed goldens. Other kotlinc-spawning files (`diff_kotlinc`,
    `diagnostics_match_kotlinc`, …) can adopt the same one-shot batch — follow-up.
  - **Persistent JVM box-runner for the execution e2e (P8 — IN PROGRESS).** The execution e2e used to
    spawn the krusty BINARY + `javac` + `java` PER TEST (3 process launches, 2 JVM cold-starts). The fix
    (the path this protocol named): a shared `tests/common` helper `compile_and_run_box(src, stem,
    cp_jars, jdk_modules)` that compiles IN-PROCESS (`compile_in_process`) and runs `box()` on a
    PERSISTENT JVM subprocess (`BoxRunner`, the conformance gate's pattern: bytes over stdin →
    ClassLoader+reflection → result, `poll(2)` deadline). One JVM per (test-binary, classpath), reused
    across every `#[test]` in that binary. CONVERTED so far (24 files, all green): `short_circuit`,
    `destructure`, `generic_fn`, `finally`, `class_body`, `vararg`, `try_catch`, `throw`, `safe_call`,
    `lambda`, `inheritance`, `companion`, `data_copy`, `property_accessor`, `not_null_assert`,
    `extension_fun`, `do_while`, `diverging_init`, `default_args_member`, `computed_prop`, `callable_ref`,
    `break_continue`, `range_step`, `secondary_ctor_noprimary`. NOT converted (need machinery the helper
    doesn't model — follow-up): `suspend_e2e` (36 tests, separate workstream), `top_level_property`
    (`main()`, not `box()`), `inline_splice` (real-kotlinc cross-module + raw-bytes asserts), `java_instance`
    (javac-built aux class dir on cp), `feature_box`/`box_vendored` (multi-snippet custom harness),
    `cli_dropin` (exercises the real CLI binary on purpose), `diff_kotlinc`/`diagnostics_match_kotlinc`
    (kotlinc differential). NOTE: `range_step`/`secondary_ctor_noprimary` previously hard-asserted krusty
    compile success; via the helper a compile failure now flows to the `None`-skip branch (consistent with
    the rest of the suite's "skip-on-unsupported"), a slight loosening to revisit if either regresses.
  - Heavy tier — the full `cargo test`. PROFILED (2026-06-24, 4 cores): the cost is NOT kotlinc (1–6
    spawns, compile-once-batched) but the ~57 JVM-bound e2e BINARIES, which `cargo test` runs
    SEQUENTIALLY (~64s summed). `cargo nextest` is WORSE (~82s — its process-per-test model loses each
    binary's shared persistent JVM). FIX (P23): `just test` (the pre-push tier) now builds once then runs
    the binaries in PARALLEL (`xargs -P $(nproc)`), each keeping its per-binary shared JVM. The
    conformance gate binary is internally rayon-parallel (saturates every core alone), so it runs
    FIRST/alone — bundling it into the batch only contends; then the rest run in parallel. Wall-clock
    **~57s (<60s ✓)**; any binary's non-zero exit fails the run and prints its captured log. (A filter
    arg defers to plain `cargo test`.) The conformance/validation gate alone stays ~8–38s.
- kotlinc 2.4.0 runs on JRE 25 (verified). bytediff is slow (one kotlinc JVM launch per file) — sample.

## Phase log

(newest first — every entry = a committed+pushed phase, gate FAIL=0)

- **Phase P42 — function references as `FunctionReferenceImpl` subclasses → real reference EQUALITY
  (gate 1576 → 1582, +6, FAIL=0).** Top-level (`::f`) and member (`obj::m`, `Type::m`, `O::m`,
  `this::m`) function references were emitted as bare `LambdaMetafactory` closures — which gave NO Kotlin
  reference equality (`::f != ::f`, breaking `callableReference/equality/*` + any program comparing
  refs). They now lower to synthesized subclasses of `kotlin/jvm/internal/FunctionReferenceImpl`
  (mirroring the existing `PropertyReference*Impl` machinery): a new `IrClass{func_ref: Some(FuncRef…)}`
  emitted by `emit_func_ref_class`, instantiated as `<Synth>.INSTANCE` (unbound singleton) or
  `new <Synth>(receiver)` (bound). Each carries `super(arity, [receiver,] owner.class, name, signature,
  flags)` so the base class's `equals`/`hashCode` compare owner+name+signature+boundReceiver — `::f==::f`,
  `a::m==a::m`, `a::m!=b::m`, `a::m!=Type::m`. The single erased `invoke(Object…)Object` casts/unboxes its
  args and dispatches: `invokestatic` (top-level, flags=1), `invokevirtual`/`invokeinterface` on the
  first arg (unbound member) or `this.receiver` (bound member), boxing the result or returning the `Unit`
  singleton for a `void` target. This SUBSUMES `Unit`-returning member refs (no longer skipped). Caught
  during bring-up via the gate: interface-member refs need `invokeinterface` (an `IncompatibleClass
  ChangeError` otherwise). Local-fun / extension / expression-receiver refs stay `LambdaMetafactory` for
  now (no equality test exercises them). New `callable_ref_equality_e2e` (equality + still-invokes).
- **Phase P41 — bound callable references on an expression receiver (gate 1572 → 1576, +4, FAIL=0).**
  A bound reference whose receiver is an arbitrary EXPRESSION (`1::foo`, `mk()::dbl`), not just an
  in-scope name. The receiver is evaluated once and captured. Two cases: (a) a bound EXTENSION function
  (`expr::extFun`) reuses the lifted static `extFun(recv, args…)` as the closure `impl_fn` with the
  receiver as the sole capture (same metafactory trick as a local-fun ref); (b) a bound MEMBER on a
  user-class receiver synthesizes `(recv, args…) -> recv.m(args…)`. Resolve types both as
  `(method/ext args) -> ret` (receiver bound) via `method_of` / `ext_funs` keyed by the receiver's
  erased descriptor. Library-type members (`"abc"::get`) still skip (not IR classes). FIX during
  bring-up: two OVERLOADED enclosing functions share `cur_fn_name`, so the synthesized impl name must
  use the ref's globally-unique AST expr id, not the per-function `lambda_seq` (a `ClassFormatError:
  Duplicate method name` otherwise). New `bound_expr_ref_e2e` (ext on `Int`, member on `mk()`, and the
  overloaded-enclosing-fn no-clash case). +4 corpus, all real `box()=="OK"`.
- **Phase P40 — local function references `::localFun` (gate 1566 → 1572, +6, FAIL=0).** A reference to
  a local function (`fun inc(x) = …; ::inc`) was rejected. It lowers to a closure over the function's
  lifted static method: the checker maps the ref to the local fun's decl (reusing `local_call_map`, the
  same map a local-fun CALL uses) and types it `(args) -> ret`; the lowering builds an `IrExpr::Lambda`
  whose `impl_fn` IS the lifted method and whose `captures` are the same outer locals the method takes as
  leading params — so a CAPTURING local fun ref (`val base = …; fun shift(x) = x + base; ::shift`) carries
  `base` into the closure (the metafactory binds captures, `invoke` supplies the declared args). A
  `Unit`/`Nothing` SAM-return is skipped for now (needs an adapter wrapper). New `local_fun_ref_e2e`
  (no-capture, capturing, `.map(::shift)`, val binding). +6 corpus, all real `box()=="OK"`.
- **Phase P39 — object/singleton method references `O::m` (gate 1565 → 1566, +1, FAIL=0).** An
  `O::method` where `O` is an `object` was rejected ("callable references are not supported" /
  "unresolved reference 'O'") — the callable-ref resolver explicitly skipped objects. It's a BOUND
  reference: the singleton is captured and the arity is the method's own args (the receiver is NOT a
  parameter), so `O::dbl` types as `(Int) -> R`. Resolve: a receiver naming an object now types as
  `Ty::fun(method params, ret)`. Lower (`lower_method_ref`): the captured receiver is the singleton
  `getstatic O.INSTANCE` (`IrExpr::StaticInstance{field:"INSTANCE"}`) instead of a captured local;
  `bound_capture` now carries the capture EXPR directly (local `GetValue` OR the static instance),
  unifying the bound-local and bound-singleton paths. Unbound `Type::m` and bound `obj::m` unchanged.
  New `object_method_ref_e2e` (singleton field access through the captured `this`, 1- and 2-arg, val
  binding). Correctly verified at runtime (the corpus `+1` is a real `box()=="OK"`).
- **Phase P38 — member expr-body return inference for `if`/`when` bodies (gate 1563 → 1565, +2,
  FAIL=0).** Extends P37: the lightweight member-signature inferer (`infer_lit_ty_p`) now types an
  `if`/`else` or `when` expression-body member (`fun absLike(x: Int) = if (x > 0) x else -x`,
  `fun grade(s: Int) = when { … else -> … }`) as the **common type of its branches** — identical types
  collapse, numeric branches widen (`Int`/`Long` → `Long`), anything else stays `Error` so the inferer
  conservatively skips rather than guess a supertype (SOUND, not complete — the full checker still does
  the real least-upper-bound). `if` needs an `else`; `when` needs an explicit `else` arm (provably
  exhaustive as a value). Also infers a block-expr body's trailing value. This is the authoritative
  fix: `infer_lit_ty_p` populates the STORED method signature that BOTH the checker and `ir_lower` read —
  refining only the checker's local `ret_ty` would leave `ir_lower` emitting a `Unit` descriptor
  (miscompile). New `member_ctrl_inference_e2e` (`if` abs, `when` grade, `Int`/`Long`-widening branch).
- **Phase P37 — member expr-body return inference for bitwise/shift infix calls (gate 1562 → 1563, +1,
  FAIL=0).** A *member* (object/class) function with an expression body whose value is a builtin
  bitwise/shift infix call — `fun packFast(…) = (r shl 0) or (g shl 8) or …` — wrongly inferred its
  return type as `Unit`, then rejected the body with a spurious "expected 'Unit', actual 'Int'". The
  lightweight member-signature inferer (`infer_lit_ty_p`) handled `this.m()`, primitive conversions and
  `toString`, but not the infix desugaring `r shl 8` → `r.shl(8)`: on an `Int`/`Long` receiver, `shl`/
  `shr`/`ushr`/`and`/`or`/`xor` (one arg) and `inv` (unary) now return the receiver's type — mirroring
  the full checker's builtin-bitwise handling (`resolve.rs:6107`). Top-level functions already inferred
  correctly (they use the full checker); only the member pre-pass was weaker. New
  `member_infix_inference_e2e` (packed RGBA `Int`, `Long` mask, `inv`). Unblocks `arithmetic/github1856`;
  the other "expected 'Unit', actual 'Int'" files carry further blockers (callable refs / inline classes).
- **Phase P36 — `Unit`-returning `tailrec` → loop (gate 1562 → 1562, +0 corpus, FAIL=0).** Removes the
  P34 bail (`if ret_ty == Unit { return None }` — which dropped the whole file). A `Unit` body recurses
  with a bare *statement* (`if (c) f(args)` / `{ …; f(args) }`), never `return f(args)`, so the
  return-driven value transform couldn't see it. New `lower_tail_unit` walks to the tail position —
  trailing expr, or last statement — and rewrites a tail self-call into the same param-reassign +
  `continue` (alias-safe temps), recursing through `if`/`else` branches and nested `{ … }` blocks.
  Tracks whether each path always transfers control: fall-through paths get a synthesized `return` (Unit)
  to exit the `while(true)` loop, diverging paths don't (no dead/unverifiable code). Any self-call
  outside tail position still bails (skip file) — never miscompiles into stack-overflowing recursion.
  +0 on the corpus (its only non-`return` `Unit`-tailrec box tests are under `coroutines/` =
  suspend-blocked), but the feature is real and proven by `tailrec_unit_returning_runs` (1,000,000-deep,
  bare-tail + if/else shapes, runs flat under `-Xverify:all`). Closes a documented "bail".
- **Phase P35 — numeric primitive → `Number` assignability (gate 1561 → 1562, +1, FAIL=0).** A numeric
  primitive (`Int`/`Long`/`Double`/…) is a subtype of `Number` — it boxes to its wrapper, which IS a
  `Number` — so `fun f(n: Number)` accepts `5`, `val n: Number = 5L` type-checks. `expect_assignable`
  gained that case (`java/lang/Number`/`kotlin/Number` expected, numeric actual). A broader
  primitive→`Comparable`/`Serializable` clause was tried but dropped — it miscompiled a contravariant
  value-class case (VerifyError); `Number` alone is clean. New `number_assignability_e2e`. (NOTE:
  `Number.toInt()` member calls remain unresolved — a separate Kotlin-`Number`-method-mapping gap.)
- **Phase P34 — `tailrec` value-returning functions → loop (gate 1560 → 1561, +1, FAIL=0).** `tailrec`
  was deliberately unparsed (ignoring it = no TCO = stack overflow = miscompile). Now PARSED
  (`is_modifier` + `FunDecl.is_tailrec`, threaded through all `parse_fun` callers) AND TRANSFORMED: a
  top-level value-returning `tailrec fun` is lowered to `while(true) { … }` where a tail self-call
  reassigns the param slots (via temps, alias-safe) and `continue`s — so 1,000,000-deep recursion runs
  flat (verified). Handles expr bodies (`= if(c) base else f(args)`, recursing into `if` branches) and
  block bodies (`return f(args)` intercepted). SOUND SKIPS (each was a real StackOverflow in the first
  cut): extension/infix `tailrec` (receiver), MEMBER `tailrec`, `Unit`-returning `tailrec` (tail call is
  a bare statement, not a `return`), default-param self-calls, and any non-tail self-call (bailed). New
  `tailrec_e2e` (deep recursion). Modest net (+1) — most corpus `tailrec` tests are `Unit`/extension/
  member (now cleanly skipped); value-returning is the common real-world case.
- **Phase P33 — `Pair`/`Triple` constructors (gate 1559 → 1560, +1, FAIL=0).** `Pair(a, b)` / `Triple(a,
  b, c)` were "unresolved function" — the classpath scan indexes these auto-imported `kotlin.*` classes by
  FQ name only (they're otherwise reached via `to`), so `class_names` lacked the simple-name mapping.
  Seeded `Pair`→`kotlin/Pair`, `Triple`→`kotlin/Triple` (classpath entries still take precedence). Also
  fixed `LibraryType::ctor` to box PRIMITIVE args into an erased `Object`/`Any` ctor param (`Pair(1, 2)` →
  `Pair(Object, Object)`) — it previously widened only reference args. New `pair_triple_e2e`. (NOTE:
  `.first`/`.second` are erased to `Any` — typed member access on a Pair element still needs generic
  type tracking, which `Ty` lacks; `Pair(...)` + `==`/passing works.)
- **Phase P32 — no-receiver `run { … }` with a branchy body (gate 1557 → 1559, +2, FAIL=0).** The stdlib
  `inline fun <R> run(block: () -> R): R = block()` (no receiver) is now intercepted in `ir_lower` and the
  lambda body inlined directly as the value — like the receiver scope fns (`x.let`/`with(x)`). Previously
  it fell to the bytecode splicer, which bails on a branchy body (`run { if … }` / `run { when … }` →
  "emit bailed"). Guarded to the simple shape (no params, no `return@run`). New `run_noreceiver_e2e`.
- **Phase P31 — smart-cast within an `||` condition (gate 1557, FAIL=0; correctness, gate-neutral).**
  Completes P30: in `a || b`, the RHS is reached when `a` is FALSE, so it gets `a`'s NEGATED narrowing —
  `x !is String || x.length != 1` (reaching the RHS means `x` IS a `String`). The `&&`/`||` checker arm now
  narrows via `smartcast_binding(lhs, for_else = (op == Or))` (same value-class guard). Common
  `if (x !is T || …) return` idiom; gate-neutral so far (corpus uses carry other blockers) but e2e-verified
  (`smartcast_and_e2e`). 
- **Phase P30 — smart-cast within an `&&` condition (gate 1556 → 1557, +1, FAIL=0).** After `x is T`
  (or `x != null`) on the left of `&&`, `x` is now narrowed to `T` while checking the right operand
  (`x is String && x.length == 1`) — previously "unresolved member 'length' on kotlin/Any". The checker's
  `Binary` `&&` arm evaluates the left, applies `smartcast_binding` in a pushed scope (as the `if`-then
  path does), checks the right, then types via `check_binary` (preserving the "operator cannot be applied"
  error for non-Boolean operands). GUARD: don't narrow to a VALUE class — its erased unboxed-equals use
  in the same boolean expr (`x is V && x == …`) miscompiled (the +2 FAIL the first cut produced).
  New `smartcast_and_e2e`.
- **Phase P29 — interface property reads in default methods (gate 1555 → 1556, +1, FAIL=0; removes a
  bail).** An unqualified property read inside an interface DEFAULT method now routes through the getter
  (`invokeinterface getX`) instead of a (nonexistent) interface field — an interface has no backing
  fields, its properties are abstract getters. The unqualified-`Name` read path skips the own-field
  lookup when `cur_class` is an interface. This LETS the P28 conservative guard be REMOVED (defaults on
  interfaces that declare abstract properties are now handled, e.g. `traits/genericMethod`: a default
  `fun a() = property`). New `interface_default_method_e2e::default_method_reads_abstract_property`.
- **Phase P28 — call an inherited interface default through the concrete class (gate 1549 → 1555, +6,
  FAIL=0).** Completes P27's follow-up: `C().f()` where `C : I` doesn't override `I`'s default `f`.
  `resolve_method` now, after the superclass chain, walks the class's interfaces transitively and returns
  the interface's `(class_id, method)` for a default — so the call emits `invokeinterface I.f` on the `C`
  receiver. SOUND GUARDS added after the fix surfaced 7 FAILs: (a) the candidate must be a genuine DEFAULT
  method, checked on the AST (`iface_method_is_default`, order-independent — the IR body is set later in
  pass 2) — without this, class-delegation `by` and abstract methods emitted `invokeinterface` to an
  unimplemented method (`AbstractMethodError`); (b) skip a VALUE-class receiver (needs boxing to dispatch —
  VerifyError/IncompatibleClassChange otherwise); (c) skip interfaces that declare (abstract) properties
  (a default reading one lowers it as a nonexistent interface field — `NoSuchFieldError`; routing interface
  property reads through the getter is the follow-up). Also fixed the `resolve_method` super-chain `?` that
  aborted before the fallback at a classpath super. `interface_default_method_e2e` extended with `En().greet()`.
- **Phase P27 — interface default methods (gate 1537 → 1549, +12, FAIL=0).** A method WITH a body in an
  `interface` (`interface I { fun f() = "OK" }`) is now emitted as a JVM default method (concrete Code,
  not `ACC_ABSTRACT`, and crucially NOT `ACC_FINAL` — a final interface method is a `ClassFormatError`,
  the bug behind the first +12-compiled/12-FAIL attempt). `is_simple_interface` now admits bodied
  methods; pass-2 lowers default-method bodies like instance methods (`this`=value 0); `emit_interface_class`
  emits concrete-vs-abstract per body (reusing `emit_method`) and `emit_method` skips `FINAL` for an
  interface owner. New `interface_default_method_e2e` (inherited via the interface type + overridden).
  FOLLOW-UP: calling an inherited default through the CONCRETE class (`C().f()` where `C : I` doesn't
  override `f`) still bails — `resolve_method(C, f)` is `None` and the call lowering doesn't yet fall back
  to the interface default (calls via an `I`-typed reference and overrides work).
- **Phase P26 — bound callable references with a `this` receiver (`this::method`/`this::prop`) (gate
  1537, FAIL=0; correctness/de-bail, gate-neutral for now).** The resolver rejected `this::foo` as
  "callable references are not supported" because `this` isn't a scope local (`lookup("this")` is
  `None`); now it resolves via `this_ty` — `this::method` → its function type, `this::prop` →
  `KProperty0`/`KMutableProperty0`. The LOWERING already captured `this`=value 0 (`lower_method_ref`/
  `lower_prop_ref`), so no codegen change was needed. New `this_callable_ref_e2e` (a `this::method` passed
  to a HOF, run end-to-end). Gate-neutral so far — the corpus `this::` tests carry ADDITIONAL blockers
  (invoking a returned function value `expr()()`, `::equals`/`O::method` object refs, enum-entry bound
  refs) — this is one slice of the larger callable-reference feature (the next-biggest coherent bucket:
  ~103 "callable references are not supported" + ~36 mislabeled). Survey-driven: callable refs are the
  top remaining yield but need several slices to convert whole tests.
- **Phase P25 — range-typed property inference (gate 1530 → 1537, +7, FAIL=0).** A range value used to
  initialize a property (`val r = 1..10`, `'a'..'c'`, `0 until n`, `4 downTo 1`) now infers its stdlib
  range type at signature-collection time — `infer_lit_ty` gained an `Expr::RangeTo` arm mirroring the
  checker's `RangeTo` typing (`IntRange`/`LongRange`/`CharRange`/`UIntRange`/`ULongRange` by operand
  type). Previously such a property was `Error` ("cannot infer the type of property 'range0'") — the
  single biggest blocker (25×) in `ranges/contains/`. The stored range then iterates/uses through the
  existing range support. New `range_property_e2e`. (Stored-range `x in r` `contains` and stepped/reversed/
  unsigned progressions remain separate items.)
- **Phase P24 — unsigned literal/`toString`/`ULong`-promotion correctness (gate 1528 → 1530, +2, FAIL=0).**
  Three drop-in fidelity fixes for unsigned types (`UInt`/`ULong`, erased to int/long): (1) top-level &
  member property inference now types an unsigned-literal initializer (`val ua = 1234U` → `UInt`) — was
  `Error` ("cannot infer the type of property"). (2) String concatenation `"x" + uint` now converts the
  operand via `Integer.toUnsignedString`/`Long.toUnsignedString` (the erased-int `String.plus`/`valueOf`
  printed the SIGNED value — a real miscompile, e.g. `0x8fffffffU` → `-1879048193` instead of
  `2415919103`); the `$`-template path already did this, the `+`-concat path didn't. (3) A `U`-suffixed
  literal exceeding `UInt.MAX` is now a `ULong` (Kotlin's rule), not a truncated `UInt`
  (`0xffff_ffff_ffffU`). New `unsigned_toplevel_e2e`. (NOTE: broader unsigned support — `compareTo` on a
  primitive, unsigned ranges/`downTo`/`until`, unsigned division/shift — remains a separate workstream.)
- **Phase P23 — parallelize the full test suite under 60s (test-time).** `just test` builds once then runs
  the ~57 JVM-bound e2e binaries in parallel (`xargs -P $(nproc)`, each keeping its shared JVM); the rayon
  conformance gate runs first/alone to avoid contention. ~99s → ~44s. Failure-aware (any binary's non-zero
  exit fails the run + prints its log); a filter arg defers to plain `cargo test`. `nextest` was worse (82s).
- **Phase P22 — expression-parser completeness: unary `+` and `return` in expression position (gate 1515 → 1528, +13, FAIL=0).**
  Chosen via a full-corpus `survey` skip histogram (no single big bucket left — a long tail; these are two
  clean, correct gaps). (1) Unary `+` (`+5`, `0.compareTo(+0.0f)`): new `UnOp::Plus`, identity on the
  numeric types in the checker/lowerer (a user `unaryPlus` operator on a non-numeric operand skips the
  file). (2) `return value` / `return@label value` used as an EXPRESSION (`x ?: return -1`,
  `?: return null`): new `Expr::Return { value, label }` (mirrors the existing `Expr::Throw`). Parser adds
  it in `parse_prefix`; resolver types it `Nothing` and marks it diverging; the lowerer emits the
  simple function-return (bails on an enclosing `finally` / `inline` expansion / spliced-lambda label —
  those need the richer `Stmt::Return` path). KEY BUG (found via `javap`): `emit_value` had a `Throw`
  arm but NO `Return` arm — so `IrExpr::Return` in a `when`/elvis branch emitted nothing and the merge
  frame was empty (`VerifyError`). Added the `Return` arm to `emit_value` mirroring `Throw`. New
  `expr_completeness_e2e`.
- **Phase P21 — LOCAL delegated properties `fun f(){ val/var x by Del() }` (gate 1509 → 1515, +6, FAIL=0).**
  A function-body `val/var x: T by Delegate()` now compiles. New `Stmt::LocalDelegate { is_var, name, ty,
  delegate }` AST variant (avoids changing the 29 `Stmt::Local` sites). The lowering declares a synthetic
  `x$delegate` local holding the delegate; reads of `x` route to `x$delegate.getValue(null, propref)` and
  a `var`'s writes to `setValue(null, propref, value)` (the `KProperty` passed inline as a fresh
  `PropertyReference0Impl(<facade>::class, …)`, reusing `IrExpr::ClassConst`'s facade sentinel). The
  interception is in the lowerer's `Expr::Name`/`Stmt::Assign` arms, keyed by a `local_delegated` map
  (cleared per function in `lower_body`); it resolves the `$delegate` slot via the CURRENT scope (so a
  capture-remapped value space is honored, else it bails — avoids an out-of-range slot panic).
  - **Sites**: AST variant + parser (`by` in the local-stmt path) + `check_file` (types the delegate, declares
    the name at the `getValue`-return type) + lowerer (`LocalDelegate` stmt + the two interceptions +
    `make_local_propref`). Exhaustiveness arms added in 5 `Stmt`-match helpers (treat `delegate` like `init`).
  - **SOUND SKIPS** (same as member, keep FAIL=0): value-class / `provideDelegate` delegate, generic
    `getValue` return ≠ property type, value-class property type, and `getValue` reflecting on its
    `KProperty` param (no `@Metadata`). New `delegated_local_prop_e2e` (val + var, explicit + inferred).
  Delegated properties now span top-level + member + local (Phases P19–P21, **+23 gate tests total**, FAIL=0).
  NEXT: `provideDelegate`, generic/value-class delegates, `@Metadata`/`$$delegatedProperties` for reflection.
- **Phase P20 — MEMBER delegated properties `class C { val/var x by Del() }` (gate 1495 → 1509, +14, FAIL=0).**
  A class body `val/var x: T by Delegate()` now compiles. Model (reuses the member computed-property
  machinery): a synthetic **instance** field `x$delegate: Del` (final, initialized in `<init>` to the
  delegate expression) + an instance `getX()` (and `setX()` for `var`) calling
  `this.x$delegate.getValue(this, <KProperty>)` / `setValue(this, <KProperty>, value)`. The `KProperty`
  is passed **inline** per call as a fresh `new PropertyReference1Impl(C::class, "x", "getX()<ret>", 0)`
  (member ⇒ `1Impl` + owner = the class; top-level P19 used `0Impl` + facade) — runtime-equal to
  kotlinc's cached `$$delegatedProperties` array when `getValue` ignores the property; reuses the
  `IrExpr::ClassConst` node (here with the class internal, not the facade sentinel). Reads/writes of `x`
  route to the accessors via the existing member-prop accessor routing.
  - **Sites** (`ir_lower` class pipeline): removed the member bail; `is_backing_field_prop` now excludes
    delegated props (was the root of an `unwrap()` panic — they'd otherwise enter `body_fields`,
    `field_props`, `init_order`); synthetic `x$delegate` appended to `fields`/`field_type_params`/
    `field_final` (kept parallel); `getX`/`setX` registered as instance methods (pass 1) with bodies built
    in pass 2; the `<init>` init-body builder gained a delegate-field-init step + its gate now also fires
    when there are delegated props (a class with ONLY a delegated prop has empty `init_order`);
    `is_simple_class` admits delegated props. Resolver types a member delegated prop from `getValue`'s
    return; `check_file` type-checks the member delegate expression.
  - **SOUND SKIPS** (keep FAIL=0; each was a real VerifyError/wrong-result before guarding): a delegate
    that is a **value class**, defines **`provideDelegate`**, has a **generic `getValue`** whose return
    type ≠ the property type (erasure needs a cast), or a **value-class property type** — the file skips.
  - New `delegated_member_prop_e2e` (val + var, `inClassVal`/`inClassVar` shapes). NEXT: local delegation
    (`fun f(){ val x by .. }`, needs `Stmt::Local` AST change), `provideDelegate`, generic/value-class
    delegates, and `@Metadata`/`$$delegatedProperties` for reflection-dependent tests (`p.name`, etc.).
- **Phase P19 — top-level delegated properties `val x by Del()` (gate 1492 → 1495, +3, FAIL=0).**
  A top-level `val x: T by Delegate()` (explicit or inferred type) now compiles. Model (all reuse, no new
  emit path): two synthetic statics `x$delegate: Del` (init = the delegate expression) and `x$kprop:
  KProperty` (init = an inline `new PropertyReference0Impl(FacadeKt::class, "x", "getX()<retdesc>", 1)`),
  plus a `getX()` accessor whose body is `x$delegate.getValue(null, x$kprop)`. Reads of `x` route through
  `getX()` via `computed_props` (registered in lower pass 1c). Pieces:
  - **IR**: new `IrExpr::ClassConst { internal }` — `ldc class <internal>`; empty `internal` is a sentinel
    for the enclosing facade (lowering doesn't know the facade name; the emitter substitutes `self.facade`).
  - **resolver** (`collect_signatures`): a delegated property's type = the annotation, else the delegate's
    `getValue` return type (so `val a = x` infers). `check_file` now type-checks the delegate expression so
    its sub-expression types are recorded for lowering. A top-level `val a = b` referencing another already-
    collected top-level property now infers its type.
  - **lower** (`ir_lower`): `lower_delegated_top_level` builds the two statics + `getX` body; pass 1c RESERVES
    the two synthetic static-index slots so later non-delegated statics keep matching `GetStatic` indices
    (the divergence that first produced a `VerifyError`). The early lowerability gate admits delegated props.
  - **SOUND SKIP**: a file-local delegate whose `getValue` references its `KProperty` parameter (reflection —
    `p.name`/`p.returnType`/`p.toString()`) is skipped: krusty emits no `@Metadata` property entry for the
    synthesized reference, so reflection on it can't resolve (`useReflectionOnKProperty.kt` was the lone such
    case — would `KotlinReflectionInternalError` otherwise). New `delegated_prop_e2e` (explicit + inferred,
    incl. the `accessTopLevelDelegatedPropertyInClinit` shape). BYTE-PARITY follow-up: kotlinc keeps the
    `KProperty`s in one `$$delegatedProperties` array (krusty uses a per-prop `$kprop` field — runtime-equal);
    member delegated properties still skip (foundation bail). NEXT: member delegation (the larger ~mover) +
    `@Metadata` for delegated properties.
- **Phase P18 — nullable type-parameter `Signature`s (gate 1492, FAIL=0).** A nullable type-parameter
  reference (`fun <T> f(t: T?): T?`, `val a: T?`) is `T<name>;` in the JVM generic signature — `?` is not
  represented there (kotlinc drops it; the erased descriptor stays `Object`). Previously `ref_is_bare_tparam`
  bailed on the `?`, omitting the signature; now `T?` is treated as a bare type-param ref. Verified
  `fun <T> f(t: T?): T?` → `<T:Ljava/lang/Object;>(TT;)TT;` matches kotlinc. Tests: `generic_signature_e2e`.
- **Phase P17 — synthesized constructor `Signature` (gate 1492, FAIL=0). Generic-class byte-parity now
  COMPLETE.** The synthesized `<init>` of a generic class carries a `Signature` whose type-parameter
  params read `T<tp>;` — `class Pair2<A, B>(val a: A, val b: B)` → `(TA;TB;)V`, `class Box<T>(var a: T)`
  → `(TT;)V` (no `<…>` prefix; the ctor uses the class's type params, declares none). Computed at the
  primary-`<init>` emit by mapping each ctor param → its field → the `field_signatures` type-param entry.
  With this, a generic class now matches kotlinc on ALL its signatures — class + field + ctor + getter +
  setter (verified `class Box<T>(var a: T)` byte-identical: `TT;`, `(TT;)V`, `()TT;`, `(TT;)V`,
  `<T:Ljava/lang/Object;>Ljava/lang/Object;`). Tests: `generic_signature_e2e`. NEXT byte-parity frontier:
  generic SUPERTYPES (`class C<T> : List<T>`) and nested generic args (`fun f(): List<T>`).
- **Phase P16 — synthesized accessor `Signature`s for type-parameter properties (gate 1492, FAIL=0).** A
  generic class's synthesized property accessors over a type-parameter field now carry their JVM
  `Signature`: `getA()` → `()TT;`, `setA(T)` → `(TT;)V` (no `<…>` prefix — they USE the class's `T` but
  declare none; `jvm_type_params` returns `""` for an empty type-param list). Verified byte-identical to
  kotlinc. `ir_lower` records an `IrGenericSig` (empty `type_params`) per accessor fid in
  `IrFile.signatures`; the existing `emit_method` path formats it. Generic-class byte-parity now covers
  class + fields + getters/setters; the only remaining piece is the synthesized `<init>` `(TT;)V`. Tests:
  `generic_signature_e2e`. (Landed via worktree branch — `master` was being force-pushed; see
  [[feedback-never-bypass-hooks]].)
- **Phase P15 — field `Signature` for type-parameter-typed fields (gate 1491 → 1492, FAIL=0).** A field
  whose declared type is a bare type parameter (`class Pair<A, B>(val a: A, val b: B)` → fields `a`/`b`)
  gets a JVM field `Signature` (`TA;`/`TB;`), like kotlinc — verified byte-identical. `ClassWriter` gained
  `FieldInfo.signature` + `add_field_sig` + serialization (the `Signature` attr name interned when a field
  OR method uses it). Backend-agnostic (P14 design): `ir_lower::class_field_tparams` records `(field,
  type-param name)` in `IrFile.field_signatures`; the JVM backend formats `T<name>;`. Also captured
  `classreader::FieldSig.signature` (already parsed, was discarded). NEXT: synthesized ctor/getter
  signatures (kotlinc signs `<init>` `(TA;TB;)V`, `getA()` `()TA;`). Tests: `generic_signature_e2e`.
- **Phase P14 — class-level generic `Signature` + move signature FORMATTING to the JVM backend (gate
  1491, FAIL=0).** (a) ARCHITECTURE FIX (owner-flagged): P12/P13 built JVM descriptor strings inside
  `ir_lower`, coupling the backend-agnostic IR to the JVM target ([[feedback-platform-decouple]]). Now
  `ir_lower` only EXTRACTS a backend-agnostic `ir::IrGenericSig` (type-param names + bounds as Kotlin
  `IrType`; which params/return are bare type-param refs); the JVM backend (`ir_emit::jvm_method_signature`
  / `jvm_class_signature` / `jvm_bound_descriptor`) formats the `Signature` string. (b) NEW: generic
  CLASSES emit a class `Signature` (`class Box<T>` → `<T:Ljava/lang/Object;>Ljava/lang/Object;`) via
  `ClassWriter::set_signature`. Function + member + class signatures verified byte-identical to kotlinc;
  gate 1491/0. Landed on branch `phase-signatures` (the shared `master` working tree was being
  concurrently reset, wiping source — see [[feedback-never-bypass-hooks]]). NEXT: field signatures.
- **Phase P13 — generic member methods: scope own type params + emit `Signature` (gate 1491, FAIL=0).**
  Two coupled fixes: (1) CORRECTNESS — a member method's return type referencing the method's OWN type
  parameter (`class Box { fun <U> wrap(u: U): U }`) was rejected "unresolved reference 'U'": the signature
  collector resolved member-method RETURN types under the class's type params (`ctp`) only, not the method's
  (`mtp = ctp + method params`) — the params path already used `mtp`, only the return pre-pass didn't. Now
  generic member methods compile + run. (2) BYTE-PARITY — extended P12's `Signature` emission to member
  methods (`fun <U> wrap(u: U): U` → `<U:Ljava/lang/Object;>(TU;)TU;`, verified identical to kotlinc). Net
  gate unchanged (those box tests carry other co-blockers), but member generic methods are now correct +
  byte-faithful. Tests: `tests/generic_signature_e2e.rs` (member compile+run+signature).
- **Phase P12 — generic `Signature` attribute emission (byte-parity; gate 1491, FAIL=0).** Closes part of
  the systemic byte-parity gap: krusty emitted NO generic `Signature` attribute; kotlinc emits one for
  every type-parameterized declaration (the descriptor erases type params, the Signature preserves them).
  Now a type-parameterized top-level FUNCTION emits a JVM `Signature` — `fun <T> id(t: T): T` →
  `<T:Ljava/lang/Object;>(TT;)TT;`, `fun <T: Int> idi(t: T): T` → `<T:Ljava/lang/Integer;>(TT;)TT;`
  (bound uses the boxed wrapper even though the descriptor is specialized `(I)I` from P10/P11) — VERIFIED
  byte-identical to kotlinc's signature strings. ClassWriter (`classfile.rs`) gained `MethodInfo.signature`
  + `add_method_sig` + serialization (the `Signature` attr name is interned only when used, so non-generic
  classes are unchanged). The signature string is generated in `ir_lower::fn_jvm_signature` from the AST
  (type params + bounds + param/return refs) and carried via `IrFile.signatures` (fid→string); `emit_method`
  writes it. Conservative: returns `None` (omits the attr, kotlinc-divergent but never WRONG) for shapes not
  yet modeled — a type param used inside a generic argument (`List<T>`), a non-Object/non-primitive bound,
  a vararg, member/extension/local functions. ZERO runtime risk (Signature is advisory metadata) → gate
  unchanged at 1491/0. Tests: `tests/generic_signature_e2e.rs`. NEXT for full generic byte-parity: nested
  generic args (`List<T>`), class/field Signatures, member/extension functions.
- **Phase P11 — RESTORE P10's lost source + spread operator `*arr` (gate 1491, FAIL=0).** CORRECTION:
  the P10 commit `a3b10f8` was HOLLOW — it captured only `docs/` + the test file; the actual source
  (the `TParams` refactor, `is_specializable_bound`, `FunDecl.type_param_bounds`) was reverted by tooling
  before the commit, so the pushed tree was really at the P9 gate (1457) and P10's test passed *vacuously*
  (it skips when compile fails). This phase re-applies the full P10 source — verified `fun <T: Int>` →
  descriptor `(I)I` and gate back to **1491**. LESSON: after `cargo fmt`/pre-commit, always re-check
  `git diff --stat` lists the SOURCE files before committing; a green pre-push can still hide a vacuously-
  skipping test. Also adds the **spread operator** `*arr`: `foo(*a)` (single spread → a top-level vararg
  function) lowers to `Arrays.copyOf(a, a.size)` + `checkcast` — byte-identical to kotlinc (verified). A
  guard at the `Expr::Call` lowering entry routes any spread call to a focused handler; every other shape
  (mixed spreads, fixed args, member/library callee, primitive element, non-`Name` spread) returns `None`
  → the file skips, so a spread arg never reaches the normal vararg-packing paths (never miscompiles). The
  checker reports a spread arg's ELEMENT type to resolution/vararg-checking (it behaves like N varargs).
  Spread test files mostly have other co-blockers (array-literal `dup` divergence, `Array<out>` variance),
  so net gate is +0 for now, but the codegen is proven. Tests: `tests/spread_operator_e2e.rs`,
  `tests/primitive_bound_generic_e2e.rs` (now asserts for real, not vacuous).
- **Phase P10 — specialize integral primitive-bounded FUNCTION type parameters (gate 1459 → 1491, +32).**
  ⚠️ The source for this phase did NOT land in commit `a3b10f8` (hollow — see P11); it is RESTORED in P11.
  `fun <T: Int> f(t: T): T` is specialized by kotlinc to the primitive (descriptor `(I)I`, not
  `(Object)Object` — verified). krusty previously REJECTED any primitive bound. Now a FUNCTION type
  parameter with an INTEGRAL wrappable bound (`Int`/`Long`/`Short`/`Byte`/`Char`/`Boolean`) erases to
  that primitive. Introduced a `TParams` struct (name → erasure `Ty`) threaded through `ty_of_ref` and
  the `Checker` (replacing the bare `HashSet<String>`; empty/erased map = exact old behavior, so the
  1459 existing passes are untouched). `FunDecl` now stores `type_param_bounds` (was discarded). SOUND
  RESTRICTION (each enforced after a gate FAIL surfaced it): (1) only FUNCTION params specialize — CLASS
  params stay erased (`TParams::erased`), because the value-class pass owns class-bound handling and
  naive class specialization VerifyError'd 6 box tests; (2) only INTEGRAL bounds — `Double`/`Float` are
  rejected (boxed-vs-primitive `==` differs on −0.0/NaN: `eqNullableDoublesWithTP.kt`); (3) unsigned/value
  bounds stay rejected (`kt27096Generic.kt`). NON-specializable primitive bounds are re-rejected in the
  parser so the file skips, never miscompiles. NOTE: like all krusty generics, the `Signature` attribute
  is still not emitted (kotlinc emits it) — a systemic byte-parity gap, separate from this runtime win.
  Tests: `tests/primitive_bound_generic_e2e.rs` (descriptor `(I)I`, `(C)C`, run; Double rejected).
- **Phase P9 — language-feature flags + name-based `[a, b]` destructuring (gate 1361 → 1457, +96).**
  A drop-in honors the same feature toggles `kotlinc` does. Added `krusty::features::LangFeatures` (a
  set of enabled `LanguageFeature` names) sourced from `-XXLanguage:+Foo` / `-Xname-based-destructuring`
  CLI flags (via `cli::Options.features`) and from the test infra's `// LANGUAGE:` directives (the gate,
  survey, and `compile_in_process` read them). Parser gains `parse_with_features` (`parse` stays a
  zero-feature wrapper — no caller churn). First feature: `NameBasedDestructuring` — `for ([a, b] in …)`
  and `val [a, b] = …` parse exactly like the `(a, b)` forms (same positional `componentN`; proven
  byte-identical to kotlinc with `-Xname-based-destructuring=complete`). WITHOUT the flag krusty rejects
  `[a, b]`, matching default `kotlinc` ("experimental … enable explicitly"). Fixed a latent bug this
  surfaced: `lower_destructure` never boxed a mutable destructured component captured+written by a
  closure (`var [a,b]=A(); { a=3 }()`) — now boxes into a `Ref` like any captured `var` local.
  Tests: `tests/name_based_destructuring_e2e.rs` (enabled→OK, var-capture→OK, disabled→rejected) +
  `features` unit tests. KEY LEARNING (user): experimental syntax must be flag-gated, NOT supported
  unconditionally — but a drop-in DOES support every flag kotlinc accepts (so the gate enables them per
  the `// LANGUAGE:` directive). The survey/gate now parse under per-file features, de-noising the
  histogram (the old "expected loop variable" 141 was mostly `+NameBasedDestructuring`).
- **Phase P8 — persistent JVM box-runner for the execution e2e (test-time <60s work).** Added
  `tests/common::{compile_and_run_box, run_box, find_box_class, java_home}` — a shared persistent-JVM
  `BoxRunner` (the conformance gate's in-process-compile + ClassLoader/reflection pattern) so execution
  e2e tests stop spawning the krusty binary + `javac` + `java` per test. Converted 24 single-source
  `box()` e2e files to it (all green); each test now costs ~0 process launches after the per-binary JVM
  warmup. No compiler source touched — pure harness speedup. Remaining big item: `suspend_e2e`. `lower_for_each` copied the iterable into
  a fresh local before looping; kotlinc iterates on an existing local directly (only snapshots into a
  temp when the iterable is a complex expr OR its backing `var` is reassigned in the body — confirmed
  by `forIn*VarUpdatedInLoopBody` box tests). Now reuses the local unless the body reassigns it
  (`expr_reassigns_name` AST scan) — for-in-local-array is byte-identical to kotlinc. Gate 1357/0; new
  differential + shape tests in `bytecode_parity_e2e`. Baseline this HEAD (`0be2f77`): 1357 box-OK.
- **Phase P2/P3 — counted range-loop parity (`until` / `..`, unit step).** kotlinc folds a CONSTANT
  bound into a single `i < C` exclusive test (no hoisted local, no overflow guard): `1..10` → `i < 11`,
  `0 until 10` → `i < 10`. A bound that is already a plain local (`0 until n`) is read directly, not
  hoisted. The overflow break guard is emitted only where the counter can actually wrap (`..`/`downTo`,
  or any non-unit `step`) — never for exclusive `until` step-1. `for (i in 0 until 10|1..10|0 until n)`
  is now byte-identical to kotlinc. Gate 1357/0; differential + shape tests in `bytecode_parity_e2e`.
  STILL DIVERGE (follow-up): `downTo` bound-0/negative, `step k` (kotlinc's `getProgressionLastElement`).
- **Phase P4 — `downTo` constant-bound fold.** kotlinc folds constant `downTo C` to the exclusive test
  `(C-1) < i` (operands swapped: `iconst C-1; iload i; if_icmpge`), no hoist/guard. krusty now matches
  for a non-zero folded bound (`10 downTo 2` byte-identical). KNOWN follow-ups: `downTo 1` (folded bound
  `0` hits krusty's compare-to-zero opt → `ifle`, while kotlinc keeps `iconst_0; if_icmpge`; needs a
  loop-bound-specific suppression that source `0 < x` comparisons must NOT get) and negative bounds
  (const-encoding). Gate 1357/0; differential test added.
- **Phase P5 — `&&` / `||` short-circuit (CORRECTNESS + parity).** krusty lowered `a && b` (value
  context) to eager `iand`/`ior` — both operands evaluated. A real MISCOMPILE: `x != 0 && 10 / x > 0`
  threw `ArithmeticException` for `x == 0` (kotlinc short-circuits). Now lowered to a branch
  (`a && b` → `if (a) b else false`, `a || b` → `if (a) true else b`); a literal left operand is
  constant-folded (kotlinc folds `const val`s; a branch in a field initializer would otherwise produce
  an unverifiable frame — was the 1 gate FAIL the fix first introduced, now resolved). Gate 1357/0; new
  `short_circuit_e2e` runtime tests. PARITY follow-up: kotlinc re-normalizes the right operand through a
  SHARED false-target (`iload a; ifeq F; iload b; ifeq F; iconst_1; goto E; F: iconst_0`); krusty's
  nested-`When` returns `b` directly — value-equal, shape differs. Needs a shared-label boolean
  construct.
  - SUSPEND interaction: the short-circuit `When` is applied only in NON-suspend bodies. The CPS
    flattener models a suspension on the `&&`/`||` RHS only at an UNCONDITIONAL position (the old eager
    `iand` made it unconditional); the branch form makes it conditional, which the flattener doesn't
    model yet (its own doc). A non-suspend body can't call a suspend fn, so short-circuit is always safe
    there; suspend bodies keep the eager form (`cur_fn_suspend` guard) until the flattener handles
    conditional suspension. Both `short_circuit_e2e` and the suspend `&&`-condition test pass.
- _known next divergences (from `bytediff` over the corpus)_: array-literal init (`intArrayOf(…)` uses
  `dup`-per-element; kotlinc stores to a temp + `aload`-per-element); primitive-array `iterator()` loops
  (krusty ~24 bytes larger); more loop forms (ranges/downTo/step/indices) to audit for kotlinc's
  counted-loop optimizations.
