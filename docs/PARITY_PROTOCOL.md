# krusty → kotlinc parity protocol

Goal (session directive): finish the Kotlin→JVM compiler rewrite in Rust as a **drop-in replacement for
kotlinc**. No compiler-plugin/extension support; every other compiler part must work. The produced
**bytecode must equal** the reference kotlinc's. Validate against the conformance tests in
`~/external-projects/kotlin`. Maintain our own test suite. Commit + push after each phase. Keep test
execution **< 60s** (profile/optimize otherwise). No hacks/workarounds/bails. TDD.

## Definitions of done

- **Runtime correctness**: `box()=="OK"` under `-Xverify:all` on the codegen/box corpus (the `kotlin`
  repo's `compiler/testData/codegen/box`). Current gate: **1810 OK / 0 FAIL** (scanned 7351, Phase 468).
- **Bytecode parity**: per-class `javap -c -p` normalized-equal vs kotlinc (`src/bin/bytediff.rs`).
  Normalization removes only semantics-preserving noise (source banner, instruction offsets,
  constant-pool index tokens). This is the harder bar the goal now demands.

## Tooling

- Normal full-suite gate: `./run-tests.sh` (or `just test`). No parameters are normally needed: the
  harness provisions the reference Kotlin compiler and box corpus when `just` is available, uses the
  fast `gate` profile, builds test binaries once, runs the internally parallel conformance binary
  alone, then runs the remaining binaries in parallel. Do not use `--release` for tests; the longer
  build cycle costs more than the faster run saves. See `docs/TEST_HARNESS.md` for the canonical
  agent-facing command reference.
- Harness profiling: full `./run-tests.sh` prints the slowest test binaries. For compiler-only
  conformance profiling, use
  `KRUSTY_NO_RUN=1 KRUSTY_FLAMEGRAPH=1 ./run-tests.sh --test kotlin_box_ir_jvm_conformance -- --nocapture`;
  it writes `target/flamegraph.svg` and prints phase timing. Avoid ad hoc JVM launchers in tests:
  `tests/common::compile_and_run_box`, `tests/common::run_box`, and `tests/common::javac_run` already
  reuse persistent JVM runners/servers.
- Focused conformance gate: `cargo test --test kotlin_box_ir_jvm_conformance --profile gate`
  (box()=OK, FAIL=0).
- Bytecode diff: `cargo run --release --bin bytediff -- <box_dir> [limit] [--samples]` with
  `KRUSTY_KOTLINC` (`.kotlinc/2.4.0/kotlinc/bin/kotlinc`), `KRUSTY_SURVEY_STDLIB`,
  `KRUSTY_SURVEY_JDK_MODULES` (`$JAVA_HOME/lib/modules`), `JAVA_HOME`.
- Reference repo: `~/external-projects/kotlin` (5.6G full Kotlin source; box corpus mirrored under
  `.kotlin-box/<ver>/compiler/testData/codegen/box`). VERIFIED byte-identical to
  `~/external-projects/kotlin/compiler/testData/codegen/box` (same 174 dirs; sample-file `diff` empty) —
  the gate validates against the directive's conformance tests, just via a stable local mirror so a
  `git checkout` of the 5.6G source can't shift results mid-run.

## Constraints / open items

- **Test time < 60s** — posture: the focused correctness gate is already <60s; the full self-provisioned
  suite currently exceeds that budget and should be profiled through the harness output, not plain
  `cargo test`.
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
  - Heavy tier — the full self-provisioned harness. PROFILED (2026-06-27, 4 cores): the cost is now
    dominated by JVM-bound e2e binaries, especially `serialization_krusty_only_e2e`, `suspend_e2e`,
    and `bytecode_parity_e2e`. `./run-tests.sh` / `just test` builds once, runs the internally
    rayon-parallel conformance gate alone, then runs the remaining binaries in slow-first parallel
    order (`KRUSTY_TEST_JOBS`, default `nproc`) with `--test-threads=1` per binary to reduce JVM
    contention. The harness prints a slowest-binaries table after every full run; use that as the
    profiling entrypoint. Current best observed no-op-build wall clock on this machine is **~91s**
    after moving the serialization e2e back onto the shared JVM runner; `KRUSTY_TEST_JOBS=2` was worse
    at **~151s**, so lower parallelism is not currently the fix. Any
    binary's non-zero exit fails the run and prints its captured log. A filter arg defers to plain
    `cargo test --profile gate` for focused runs.
- kotlinc 2.4.0 runs on JRE 25 (verified). bytediff is slow (one kotlinc JVM launch per file) — sample.

## Phase log

(newest first — every entry = a committed+pushed phase, gate FAIL=0)

- **Phase 468 — constructor default args captured FILE-INDEPENDENTLY (no cross-file `ExprId`); gate 1810,
  FAIL=0 (L2 itself +0 functional — the baseline rose to 1810 via merged interface-default/annotation work).** `ClassSig.ctor_defaults` stored each default as an `ExprId` into the DEFINING file's
  expr-arena. When a DIFFERENT file fills that default — a cross-file constructor call (the multi-file /
  `// WITH_COROUTINES` shape) — the checker (`check_call` ok-arity) and the `super(…)` default-fill
  dereferenced that id against the WRONG (caller's) arena and PANICKED (`ast.rs` index-out-of-bounds). Now
  `ctor_defaults: Vec<Option<CtorDefaultValue>>` — a file-independent value (literals + an `Object(internal)`
  singleton for `= EmptyCoroutineContext`-style defaults, resolved at collect via the type universe). The
  checker matches on it (no arena deref); the regular-class and companion `super(…)` default-fills build the
  `Const`/`getstatic …INSTANCE` directly (`ctor_default_to_ir`). Same-file behavior is byte-identical
  (Phase 464/466 e2e + the gate). Cross-file now SKIPS cleanly instead of panicking (cross-file class
  *construction* is itself still unmodeled, so the file skips — never a crash, never a miscompile). This is
  the L2 layer of the coroutine blocker stack (the checker-side cross-file panic) — see
  [[coroutine-intrinsics-plan]]. TDD: tests/cross_file_ctor_default_e2e.rs (a two-file module compiled with
  shared signatures — a cross-file `Base()` with a literal AND an object-singleton default completes without
  panicking).

- **Phase 467 — emit fail-soft: a `GetValue`/`SetValue` of an unallocated value slot skips the file, never
  panics (gate 1782, flat, FAIL=0).** The JVM emitter indexed `self.slots` directly (`self.slots[i]`) for
  `GetValue`/`SetValue`; if a lowering ever produced malformed IR — a value index whose slot was never
  allocated (e.g. an unsupported multi-parameter suspend-lambda shape that the state-machine lowering fails
  to bail on in the multi-file path) — the compiler PANICKED ("no entry found for key"), aborting the whole
  run instead of skipping that one file. A compiler must never crash on its own IR. Now a missing slot sets
  a thread-local `EMIT_BAIL` (mirrors the existing `INLINE_BAIL` un-spliceable-call path) and `emit_all`
  returns `None`, so the file is skipped — never miscompiled, never a crash. Pure safety net: at the current
  gate no file triggers it, so the result is byte-identical (1782/0 flat). This is the foundational hardening
  for the coroutine blocker-stack work (a 5-layer effort: L1 suspend-codegen slots, L2 checker cross-file
  ExprId, L0/L3 generic anonymous-object capture, L4 the `Continuation()` factory) — see
  [[coroutine-intrinsics-plan]]. TDD: a unit test (src/jvm/ir_emit.rs `fail_soft_tests`) feeds a hand-built
  `IrFile` whose `box()` body is `GetValue` of an undeclared slot and asserts `emit_all` returns `None`.

- **Phase 466 — check primary-constructor parameter defaults → object-singleton defaults lower (gate 1782,
  flat, FAIL=0).** A constructor parameter whose default is an OBJECT singleton (`val ctx: CoroutineContext
  = EmptyCoroutineContext` — a classpath `object` read as `getstatic …INSTANCE`), rather than a simple
  literal, could not be lowered: the checker never visited primary-ctor parameter defaults, so no
  object-value reference was recorded, and the `super(<defaults>)` default-fill (Phase 464/465) bailed on a
  non-literal default. The checker now type-checks each primary-ctor parameter default in a fresh scope
  (mirroring `check_fun`'s default pass — same lambda-default handling for a function-typed parameter),
  recording its sub-expression types and object-value references. The coroutine `EmptyContinuation`
  (`open class EmptyContinuation(override val context: CoroutineContext = EmptyCoroutineContext) :
  Continuation<Any?> { companion object : EmptyContinuation() … }`) now compiles AND runs (its self-ref
  companion is built via the Phase 465 super-default-fill). Gate is FLAT: this is prerequisite
  infrastructure for the `// WITH_COROUTINES` cluster, which the harness compiles together with an injected
  `helpers` source (`CoroutineUtil.kt`) — that file still needs three things before the ~123-file
  `EmptyContinuation` cluster counts: the `Continuation(context) { … }` stdlib factory function, an
  anonymous `object : Continuation<T>` using the enclosing function's type parameter, and the captured
  lambda-param invocation inside it. TDD: tests/object_default_ctor_arg_e2e.rs (subclass + self-referential
  companion both fill an `EmptyCoroutineContext` base default and run). See [[coroutine-intrinsics-plan]].

- **Phase 465 — `companion object : Base()` extends a base CLASS (gate 1781 → 1782, +1, FAIL=0).** A
  method-less or method-ful companion that declares a base class is now emitted extending that base, so the
  companion — used as a value (`C` denotes its companion instance) — is genuinely an instance of `Base`.
  `ir_lower` synthesizes the `C$Companion` for a companion that has methods OR a base class OR interface
  supertypes, and builds the `super(…)` call by filling the base ctor's default-value exprs (mirrors the
  Phase 464 regular-class super-default-fill). Modeled only for a SAME-FILE non-interface base with no
  explicit base args and a no-arg / all-defaulted ctor; any other base (classpath, required param, explicit
  args) bails the whole file (skip, never miscompile). The `resolve` registration of the typed
  `C$Companion` is gated to EXACTLY that same shape (same-file non-interface base) so a *different* file
  can't type-check a use of a companion that has no class file behind it (would `NoClassDefFoundError`).
  This is the regular-class half of the coroutine `EmptyContinuation` completion; the self-referential
  companion (`companion object : EmptyContinuation()`) and the classpath-object default
  (`= EmptyCoroutineContext`) remain (next). TDD: tests/companion_supertype_e2e.rs (no-arg + default-param
  base, used as their base-class value; round-tripped). See [[coroutine-intrinsics-plan]].

- **Phase 464 — `super(…)` default-fill for a class/object extending an all-defaulted base (gate 1777 →
  1781, +4, FAIL=0).** `class B : A()` / `object O : A()` where `A(val x = …)` has all-defaulted ctor
  params and no explicit base args were written — krusty bailed ("not supported by the IR backend": the
  `super()` arity didn't match A's primary ctor). It now fills A's default-value exprs into the `super(…)`
  call (the same defaults a `new A()` construction fills at the call site — krusty has no synthetic
  `$default` ctor). Gated to the clean shape (no explicit base args + every base param has a default, read
  from the base's resolve `ClassSig.ctor_defaults`); the defaults lower with only `this` in scope, so a
  default referencing a base parameter fails to resolve and bails (skip, never miscompile). Also a step
  toward the coroutine `EmptyContinuation` companion completion. TDD: tests/super_default_args_e2e.rs
  (class + object extending an all-default base; round-tripped). See [[coroutine-intrinsics-plan]].

- **Phase 463 — `startCoroutine` + suspend-lambda `create()` + Unit-lambda return + classpath body-property
  type (a full coroutine runs end-to-end; gate 1777, box-OK flat, FAIL=0).** Four pieces, completing the
  plain-completion coroutine path (verified: `c.startCoroutine(Done())` over a suspend lambda runs to
  `"OK"` on the JVM): (1) `startCoroutine` — checker (extension on a `Ty::Fun` receiver via the coroutine
  registry → `Unit`) + lowering (`invokestatic kotlin/coroutines/ContinuationKt.startCoroutine(Function1,
  Continuation)V`); (2) the synthesized suspend lambda now emits the `SuspendLambda` factory
  `create(value.., Continuation): Continuation` (= `new This(captures.., completion)`) that
  `startCoroutine`/`createCoroutine` invoke (the base throws "not overridden"); (3) a non-suspending
  `suspend () -> Unit` lambda's `invokeSuspend` runs the body for effect then returns `Unit.INSTANCE`
  (boxing a Unit/no-value body `areturn`'d an empty stack → VerifyError); (4) **general fix** — a body
  property declared with a CLASSPATH type (`override val context: CoroutineContext`) now uses the
  classpath-aware `field_ty` instead of `ty_of` (which erased it to `Object`), so its field + getter keep
  the concrete type and match an overridden interface member (was AbstractMethodError). Corpus coroutine
  tests use the `EmptyContinuation` companion completion, which additionally needs companion-extends-class
  (next) — so box-OK is flat, but the plumbing is proven. TDD: tests/coroutine_intrinsics_e2e.rs.

- **Phase 462 — class name used as a value resolves to its `companion object` (gate 1777, box-OK flat,
  FAIL=0).** A class `C` whose companion declares interface supertypes and has methods (so the backend
  emits the `C$Companion` class + `Companion` field) used as a VALUE (`val i: I = C`) is now its companion
  instance: collect registers `C$Companion` as a typed object (interfaces from the companion's supertypes;
  `super_internal = None` to match the emitted `Any` superclass — a base class is still ignored, not
  claimed), the checker types `C` as `C$Companion` (assignable to its interfaces), and lowering reads
  `getstatic C.Companion`. Gated to exactly what is emitted (companion has interfaces AND methods), so a
  method-less / base-only companion (`companion object : Factory(1)`) isn't treated as a value — it skips,
  never `NoSuchFieldError`. This is the keystone for the coroutine `EmptyContinuation` completion once
  companion-extends-class lands. TDD: tests/companion_supertype_e2e.rs (companion used AS its interface
  value runs `"OK"` on the JVM).

- **Phase 461 — `companion object` with a declared supertype implements its interfaces (gate 1773 → 1777,
  +4, FAIL=0).** The parser DISCARDED a companion's supertype list (parser.rs "tolerate and ignore") and
  the synthesized `C$Companion` was hardcoded to extend `kotlin/Any` implementing nothing — so a
  `companion object : I` was not actually an `I` at runtime. `parse_companion` now captures the companion's
  base/super-args/interfaces (new `ClassDecl.companion_base`/`companion_base_args`/`companion_supertypes`),
  and the companion `IrClass` implements its declared interfaces. A companion with a base CLASS
  (`companion object : Base()`) still needs a `super(args)` call the registration pass doesn't build (and a
  default-param base ctor like `EmptyContinuation()` needs default-fill) — that shape bails (skip), never
  miscompiled. Next: companion-as-value (the companion modeled as a typed object) + companion-extends-class
  unblock the coroutine `EmptyContinuation` completion (~123 files) — see [[coroutine-intrinsics-plan]].
  TDD: tests/companion_supertype_e2e.rs.

- **Phase 460 — coroutine-intrinsic registry: `COROUTINE_SUSPENDED` / `suspendCoroutineUninterceptedOrReturn`
  / `startCoroutine` resolve + lower (foundation; gate 1773, box-OK flat, FAIL=0).** These are `@InlineOnly`
  stdlib declarations whose stub bodies just `throw`; the reference compiler recognizes them by FQ name
  (its intrinsics table) and emits dedicated codegen rather than calling/inlining. krusty's splice gate
  (`can_inline_call`) correctly refuses the `throw` body, so they resolved to "unresolved". New
  `jvm/coroutine_intrinsics.rs` FQ-name registry (the table only — signatures come from `@Metadata`, no
  hardcoded sigs), reached from the resolver via a new `LibrarySet::coroutine_intrinsic` (platform-neutral
  `CoroutineIntrinsic` enum; keeps `crate::jvm` out of the resolver). Checker: `COROUTINE_SUSPENDED`→`Any`,
  `suspendCoroutineUninterceptedOrReturn{…}`→enclosing suspend fn's `T` (lambda param = `Continuation`),
  `(suspend …→T).startCoroutine(completion)`→`Unit`. Lowering: `COROUTINE_SUSPENDED`→
  `IntrinsicsKt.getCOROUTINE_SUSPENDED()`; `suspendCoroutineUninterceptedOrReturn{block}`→inline the block
  (leaf shape — a block that READS its continuation bails, never miscompiles). Net box-OK is flat: a full
  coroutine `box()` round-trip additionally needs the companion-object-as-value completion (the
  `EmptyContinuation` test helper, ~123 files) — krusty's flattened-companion model doesn't yet emit a
  companion singleton WITH a supertype usable as a value; that is the next phase. This lands the prereq
  cleanly (FAIL=0, no regression). TDD: tests/coroutine_intrinsics_e2e.rs (compile-only: leaf intrinsics;
  startCoroutine on a suspend-fn value). See memory [[coroutine-intrinsics-plan]].

- **Phase 459 — generic base-member return resolved under the BASE's type params (gate 1763 → 1773, +10,
  FAIL=0).** Collecting a subclass's signatures walks the base class's methods to seed `local_rets` (so an
  inferred-body method can call an inherited one). It resolved each base method's return type under the
  SUBCLASS's type parameters — but a base method's return references the BASE's params
  (`abstract class A<T> { abstract fun f(): T }`, `class C : A<String>()`), so `T` came back "unresolved
  reference 'T'". Now resolved under the base's own (erased) params, extended with the method's own. This
  newly compiled a value-class corner (`class B2 : A<IC>(), I` where `I { fun foo(): IC }`) needing an
  interface bridge that unboxes the erased `Object` return into the value class — codegen krusty can't
  emit (VerifyError / AbstractMethodError); `check_no_bridge_needed` now rejects exactly that shape
  (supertype return is a `@JvmInline value class` while the concrete inherited return is erased
  `kotlin/Any`), skipping not miscompiling. TDD: tests/generic_base_member_type_e2e.rs.

- **Phase 458 — postfix method chain continues across a newline (correctness; gate 1763, box-OK flat,
  FAIL=0).** Kotlin treats a newline before `.` or `?.` as part of the selector chain, not a statement
  terminator (`val s = x\n  .foo()\n  .bar()`). `parse_postfix` broke at the newline, so a leading-dot
  continuation line failed to parse ("expected an expression" at the `.`). The loop now peeks past the
  newline(s); a following `.`/`?.` consumes them and continues the chain, otherwise it stops. `::` and a
  trailing `{` lambda deliberately do NOT continue across a newline (same-line only in Kotlin). Net box-OK
  is flat — the unblocked files hit downstream feature gaps — but it removes a whole parse-error class and
  regresses nothing. TDD: tests/newline_method_chain_e2e.rs.

- **Phase 457 — sealed-object `when` value-match (gate 1766 → 1767, +1, FAIL=0).** A `when (s) { A -> … }`
  over a sealed subject `s: S` that matches a singleton object subclass (`object A : S()`) by *value*
  (`==`), not `is`, failed two checks: (1) the when-comparability rule rejected the arm condition `A`
  (type `A`) as "not comparable to subject `S`" — fixed by `when_objs_comparable`, which permits two
  reference types when one is a subtype of the other (`obj_is_subtype` either direction); (2)
  `when_sealed_exhaustive` only counted `is Sub` arms, so a value-arm `when` was deemed non-exhaustive and
  typed `Unit` (→ "return type mismatch") — it now also counts a bare-`Name` arm resolving to a class
  whose internal is one of the sealed subclasses. TDD: tests/sealed_object_value_match_e2e.rs.

- **Phase 456 — `object O : Base(args)` extends a class (gate 1762 → 1766, +4, FAIL=0).** `parse_object`
  previously discarded an object's base class and super-args; it now captures them (`base_class`,
  `base_args`) like an ordinary class, so the general class lowering computes the `superclass` and emits
  the `super(args)` call. This also registers a sealed hierarchy of objects (`sealed class S; object A : S();
  object B : S()`) as subclasses of the sealed base, making a `when (s) { is A -> …; is B -> … }`
  exhaustive. `is_simple_object` no longer requires `base_class.is_none()`, but skips an object with BOTH
  a base class AND interfaces (`object O : A(), T`): that shape invites a qualified `super<A>`/`super<T>`
  call krusty doesn't yet dispatch — gated as unsupported rather than miscompiled. TDD:
  tests/object_extends_class_e2e.rs (object extends class with arg; sealed-object `when (is)`; override
  through the base type).

- **Phase P78 — `as? T ?: e` parse fix (gate 1761 → 1762, +1, FAIL=0).** An unparenthesized safe cast
  followed by elvis (`x as? String ?: "no"`) failed to parse: `parse_type` greedily ate the `?` of the
  `?:` as a nullable `String?`, leaving a dangling `:` ("expected an expression"). New `eat_type_nullable`
  helper eats a `?` nullable-type marker UNLESS it's immediately before `:` (a nullable type is never
  validly followed by `:`, so `?:` there is elvis) — applied at all type-nullable parse sites.
  `(x as? T) ?: e`, `as?` alone, and ordinary nullable types are unaffected. TDD:
  tests/safe_cast_elvis_e2e.rs (success path, failure path, nullable-type regression guard).

- **Phase P77 — enum implementing a GENERIC interface, with erased bridge (gate 1752 → 1761, +9,
  FAIL=0).** P76 gated ALL generic-interface enums (`enum class Z : A<String>`); this lands them. The
  general `Decl::Class` lowering already computes the interface bridge for an enum (it runs the
  `!is_interface` bridge pass), and `emit_enum_class` now emits `c.bridges` — so an enum-level override
  of a generic interface method (`override fun foo(t: String)`) gets the erased
  `foo(Object)`→`foo(String)` ACC_BRIDGE the JVM needs for an interface-typed call (without it,
  `(Z.Z1 as A<String>).foo("")` ran the interface default → wrong result). The P76 gate now allows a
  generic interface IFF its abstract method is overridden at the ENUM level (bridge belongs on the enum
  class); a generic method satisfied only by PER-ENTRY overrides still skips (the bridge would belong on
  each entry subclass — not modeled). +9 (a generic-interface-enum cluster, e.g. `bridges/simpleEnum.kt`).
  TDD: tests/enum_generic_interface_e2e.rs (default-overridden + abstract, called via the interface).

- **Phase P76 — enum implementing an interface (`enum class E : I`) (gate 1750 → 1752, +2, FAIL=0).**
  The enum header parser didn't accept a supertype list, so `enum class E : I { … }` failed to parse
  entirely. Now: the parser reads the optional `: I1, I2` supertype list into `ClassDecl.supertypes`; the
  checker resolves a return-type-less entry/enum `override` against the implemented interface (via
  `supertype_methods`); ir_lower's enum path looks up the override sig on the interface and the
  `is_simple_enum` gate admits interface supertypes; and `emit_enum_class` now emits the `implements`
  clause (without it an interface-typed call threw `IncompatibleClassChangeError`). The abstract interface
  member is satisfied by an enum-level method, a per-entry override, OR a default. SOUND skips (caught as
  gate FAILs mid-development, then gated): a GENERIC interface (`A<T>` — needs an erased `foo(Object)`
  bridge krusty doesn't emit), a classpath-interface supertype, and any unsatisfied abstract member
  (e.g. an interface `val ordinal` → `getOrdinal` the enum doesn't provide) all skip the file. TDD:
  tests/enum_implements_interface_e2e.rs (enum-level override, per-entry override, default method — all
  called via the INTERFACE type).

- **Phase P75 — same-file `const val` read inlining (gate 1750 → 1750, +0 corpus, byte-equality FIX,
  FAIL=0).** Completes the const byte-parity started in P73: a same-file top-level `const val` read now
  inlines its literal as `ldc` (kotlinc's behavior) instead of `getstatic`. ir_lower records each
  top-level `const val`'s compile-time literal (`const_lits`, via `ast_literal_const` narrowing to the
  declared type — `Byte`/`Short`/`Char`/etc.) in pass 1c, and the bare-name read emits `IrExpr::Const`.
  With P73's `ConstantValue` field + omitted `<clinit>`, a pure const read is now BYTE-IDENTICAL to
  kotlinc (verified: normalized `javap` diff empty for `const val X = "OK"; fun box() = X`). Remaining
  const-fold gap (separate): kotlinc folds a const-in-condition (`if (N == 42)`) to a constant branch;
  krusty still emits the runtime compare. Same-file only (cross-file/companion const reads inline as a
  follow-up — they need the classpath `ConstantValue`). +0 corpus (const was runtime-correct); pure
  byte-parity. TDD: tests/const_read_inline_e2e.rs.

- **Phase P74 — companion-object `const val` (gate 1750 → 1750, +0 corpus, FAIL=0).** A `companion
  object` with ANY property previously bailed the whole file. Now a `const val` (compile-time literal) in
  a companion is emitted as a `public static final` + `ConstantValue` field on the OUTER class — kotlinc's
  layout, reusing P73's `ConstantValue` infra — and a `C.X` read lowers to `getstatic C.X`. Pieces:
  `IrStatic` gains `owner: Option<String>` (None=facade), the lowerability gate accepts a companion whose
  props are all plain `const val` (`companion_props_lowerable`), ir_lower emits each as an owned static +
  records `companion_consts[(C, X)]` so reads resolve, the facade `emit_statics` skips owned statics, and
  `emit_class` emits them on their class. A companion with BOTH const vals AND methods works (consts on C,
  methods on `C$Companion`). Sound boundaries: a NON-const companion property (needs the `access$getX$cp`
  accessor + `Companion.getX()`) still skips; a const-only companion does not yet emit the (empty)
  `C$Companion` + `Companion` field kotlinc also produces, and reads are `getstatic` not inlined `ldc` —
  byte-parity follow-ups, gate-correct today. TDD: tests/companion_const_e2e.rs (read, int+string,
  const+method).

- **Phase P73 — `const val` byte-parity: `ConstantValue` attribute + no `<clinit>` (gate 1750 → 1750,
  +0 corpus, byte-equality FIX, FAIL=0).** krusty was NOT byte-equal to kotlinc for ANY `const val`: it
  emitted the field with no `ConstantValue` attribute and a `<clinit>` doing `ldc; putstatic`, while
  kotlinc emits the field WITH a `ConstantValue` attribute and an EMPTY (omitted) `<clinit>` — the JVM
  initializes the field from the attribute. Fix targets the directive's hard bar ("bytecode must be
  equal"): `classfile.rs` `FieldInfo` gains a `const_value` + an `add_field_const` that serializes the
  `ConstantValue` attribute; `emit_statics` emits it for a compile-time-literal `const val` and SKIPS that
  static's `<clinit>` store; when every static is so folded, NO `<clinit>` method is emitted at all.
  Verified byte-identical to kotlinc (field `ConstantValue: String OK` / `int 42`, no `<clinit>`). Const
  READS are still `getstatic` (kotlinc inlines `ldc`) — a separate, broader follow-up. +0 corpus (const
  was already runtime-correct); this is a pure byte-parity improvement. TDD:
  tests/const_constantvalue_e2e.rs (parses the facade: asserts `ConstantValue` present + no `<clinit>`).

- **Phase P72 — member/extension resolution on Kotlin MAPPED collection types (gate 1750 → 1750, +0
  corpus, byte-equal, FAIL=0).** `kotlin.collections.List`/`Set`/`Map`/`Iterable`/… have no own JVM
  `.class` — their *actual* platform declaration IS a JVM interface (`java/util/List`), the `expect`/
  `actual` + `JavaToKotlinClassMap` device kotlinc uses. `JvmLibraries::resolve_type` returned `None` for
  these (the `.class` reader `cp.find` has no entry), so NO member/extension on a `List`/`Set`/… resolved
  (`for (x in list)`, `list[i]`, `list.size`, `list.iterator()`, `forEach`/`contains`/`indexOf` all
  failed). Fix (generic, no per-type hack): when `cp.find(internal)` is `None`, fall back to the mapped
  (actual) type via the SAME generic `to_jvm_internal` device the emitter uses for the call owner — so
  resolution and codegen stay byte-consistent. Verified BYTE-IDENTICAL to kotlinc for the iterator
  protocol (`invokeinterface java/util/List.iterator()Ljava/util/Iterator;`, `Iterator.hasNext()Z`,
  `Iterator.next()Ljava/lang/Object;`). +0 on the box corpus only because the collection-heavy tests are
  ALSO gated by other features (`forInIndices` parser syntax, primitive-upper-bound type params,
  coroutines, `assertEquals`) — the collection resolution itself is now functional end-to-end and is a
  foundation those tests build on. TDD: tests/collection_members_e2e.rs (for-over-List, size+index,
  isEmpty/contains/indexOf).

- **Phase P71 — `var` extension properties (gate 1740 → 1750, +10, FAIL=0).** Builds on P70: a `var Recv.name:
  T get() = … set(v) { … }` now lowers BOTH accessors as statics — `getName(Recv): T` and `setName(Recv,
  T): Unit` — with the getter/setter bodies lowered with `this` = the receiver (param 0) and the setter's
  value as param 1. A read `x.name` → `getName(x)`, a write `x.name = v` → `setName(x, v)` (routed in the
  `AssignMember` lowering). A `var` extension property requires an explicit `set(v) { … }` body (no
  backing field to default to); without it the file skips cleanly. TDD: tests/var_extension_property_e2e.rs
  (Int get/set, String get/set).

- **Phase P70 — `val` extension properties (gate 1736 → 1740, +4, FAIL=0).** `val Recv.name: T get() = …`
  bailed at lowering: the checker already handled extension properties (`ext_props`, getter as a static
  `getName(Recv)`), but the lowerability gate rejected them and pass-1 mis-registered them as
  receiver-LESS computed props. Now a `val` extension property lowers exactly like an extension function:
  pass-1 synthesizes a static `getName(Recv): T` (FunId in `ext_prop_get_ids`), pass-2 lowers `get() = …`
  with `this` = the receiver (param 0), and a read `x.name` emits `getName(x)` (`Callee::Local`). Bare
  receiver-member access in the body (`val A.doubled get() = n*2`) works via the same `this`-scope path
  extension functions use. Unsupported shapes skip the file cleanly: a `var` extension property (custom
  setter) and an extension-DELEGATED property (`val Recv.x by …` — pass-1's delegate branch now returns
  None for a receiver prop, which fixed a pass-1/pass-2 desync panic caught during the gate run). TDD:
  tests/extension_property_e2e.rs (user-class bare member, `Int` `this`, `String`).

- **Phase P69 — user-class indexed access via `operator get`/`set` (gate 1734 → 1736, +2, FAIL=0).**
  `m[i]` / `m[i] = v` on a user class with an `operator fun get(index)` / `operator fun set(index,
  value)` was rejected ("'M' is not an array (cannot index)"): the index checker + lowering only handled
  arrays, `String`, and LIBRARY objects (via `resolve_instance` on the classpath). Now a `Ty::Obj` whose
  USER class declares `get`/`set` (resolved via `syms.method_of`, walking supers) routes `m[i]` →
  `m.get(i)` and `m[i] = v` → `m.set(i, v)`, emitted as the instance `MethodCall` (the same
  `invokevirtual` kotlinc emits — byte-faithful). Single-index `get(i)` and two-arg `set(i, v)` are
  modeled; the library path (List/Map/array) is unchanged. TDD: tests/operator_index_e2e.rs (get, get+set,
  String-key get).

- **Phase P68 — single-spread of a PRIMITIVE array into a `vararg` function (gate 1734 → 1734, +0
  corpus, byte-equal, FAIL=0).** `f(*intArrayOf(1,2,3))` / `f(*xs)` (forwarding a vararg param) bailed
  ("this construct is not yet supported") — `lower_single_spread_call` only handled REFERENCE-array
  spreads (`Object[]` `copyOf` + checkcast). A genuine JVM-primitive element now uses the matching
  `Arrays.copyOf([<prim>I)[<prim>` overload with NO checkcast (the result is already the exact array
  type). Verified BYTE-IDENTICAL to kotlinc (same `aload;ldc;checkNotNull;aload;aload;arraylength;
  copyOf([II)[I;invokestatic f;ireturn`). Unsigned `UInt`/`ULong` varargs (a `UIntArray` value-class
  array with a different copy path) still skip (sound). +0 on the box corpus (no file gates on exactly
  this shape) but a real byte-faithful capability common in practice. TDD: tests/primitive_spread_e2e.rs
  (Int literal spread, vararg-param forward, Long spread).

- **Phase P67 — properties in an enum entry body (gate 1733 → 1734, +1, FAIL=0).** `enum class E { A { val y = …; override fun f() = y }; abstract fun f(): String }` was rejected by the parser ("only method overrides are supported in an enum entry body") — only method overrides in an entry body were modeled. Now a `val`/`var` in an entry body becomes a private backing field + getter on the synthesized `E$Entry` subclass, initialized in its constructor after `super(name, ordinal[, args])`, and the override resolves the property as `this.<field>`. Pieces: parser collects entry-body props into a new parallel `ClassDecl.enum_entry_props`; the checker types each initializer and makes the entry's props visible to that entry's override bodies; ir_lower gives the entry subclass the fields + a getter per prop + an `init_body` that stores each, REGISTERS the subclass in the lowering's class map, and lowers the override bodies with `cur_class = E$Entry` (so a prop reads as a subclass field — a property-less entry keeps the enum scope, unchanged); the entry-subclass emitter now emits the fields and runs `init_body` in the ctor. Only a plainly-initialized prop is modeled (a getter/setter/delegate/`lateinit` entry prop cleanly skips). Byte-faithful (private field + `getX` on `E$Entry`). TDD: tests/enum_entry_property_e2e.rs (read-by-override, mixed prop/method entries, Int prop).

- **Phase P66 — infer a property/local type from a classpath `object` value (gate 1731 → 1733, +2,
  FAIL=0).** `val ctx = EmptyCoroutineContext` (an `object` used as a value, no explicit type) failed
  with "cannot infer the type of property". The signature-time inferer `infer_lit_ty_p` (resolve.rs)
  only typed a bare `Name` against the local property list → `Error` for a classpath singleton. Added a
  fallback: a bare name in `class_names` whose `resolve_type(internal).is_object()` infers to
  `Ty::obj(internal)` — the object's own type, the same value the full checker's `classpath_object_value`
  yields. SOUND: only an `object` is a value, so a plain class name (not a value) stays `Error` → the
  file skips, never a mistype; a current-module object isn't in the library `src` so it also stays
  `Error` (unchanged). General inference fix (not coroutine-specific). This is coroutine helper gap #4 of
  5 (see [[project-suspend]]); #1 (`Continuation()` factory), #2 (`startCoroutine`), #3 (generic `T` in
  anon), #5 (function-typed capture) still gate the 502 `WITH_COROUTINES` files. TDD:
  tests/object_value_inference_e2e.rs.

- **Phase P65 — anonymous-object capture, slice 1+2 (gate 1729 → 1731, +2, FAIL=0).** An
  `object : I { … }` expression is desugared (parser `parse_anon_object`) to a hoisted top-level synth
  class + a no-argument construction, so a body reading an enclosing local previously failed to resolve
  ("unresolved function 'x'" / "unresolved reference 'T'"). New post-parse pass `rewrite_anon_captures`
  (parser.rs, run in `parse_with_features` after `hoist_local_classes`) turns each captured enclosing
  **function parameter** and read-only **local** (type from an explicit annotation or a literal
  initializer — no inference needed) into a constructor `val` property of the synth class and passes it at
  the construction; the ordinary class machinery then resolves the body reference as a member and emits the
  field. SOUND BOUNDARIES (each stays an honest skip, never a miscompile): a captured name WRITTEN inside
  the anon (`var acc; …{ acc = … }`) needs a shared `Ref` cell (kotlinc's boxing) so it is NOT captured by
  value (`anon_body_writes` guard); a captured parameter whose type mentions an enclosing TYPE parameter is
  left alone; an outer LOCAL with a non-literal unannotated initializer (unknown type here) is left alone;
  a function-typed capture hits the pre-existing lambda→function-typed-ctor-param gap and skips. Non-
  capturing anon objects are unchanged. NEXT slices: written-`var` capture via `Ref` boxing (the common
  `object : Runnable { run() { result = … } }` shape), outer-`this`/receiver capture (`this@Outer`),
  generic/parameterized base classes, and locals with inferred (non-literal) types. TDD:
  tests/anon_object_capture_e2e.rs (param + read-only-local + non-capturing).

- **Phase P64 — faithful `// WITH_COROUTINES` helper injection (gate 1730 → 1729, −1, FAIL=0).** The
  conformance harness was treating `WITH_COROUTINES` as "add the kotlinx-coroutines-core jar" — wrong:
  kotlinc's `TestFiles.java` injects a generated `helpers` SOURCE file (`CoroutineUtil.kt`, text from
  `TestHelperGenerator.createTextForCoroutineHelpers(checkStateMachine, checkTailCallOptimization)`),
  compiled in the same module. Verified: of 502 `WITH_COROUTINES` box files, **0** import
  `kotlinx.coroutines` and **0** use `CHECK_STATE_MACHINE`/`CHECK_TAIL_CALL_OPTIMIZATION`. So krusty now
  injects the `false,false` helper variant (`EmptyContinuation`, `runBlocking`,
  `handleResultContinuation`, `handleExceptionContinuation`, `ResultContinuation`) as an extra source
  block — for both `// FILE:` and single-file coroutine tests. **Net −1**: one `// FILE:`+coroutine test
  previously compiled a helper-free subset and "passed"; under kotlinc the helper is always present, and
  krusty cannot yet compile it, so the honest result is now a SKIP (a corrected false positive). This
  un-masks the real blocker for all 502 coroutine tests: krusty can't compile the helper. The suspend
  STATE MACHINE exists (`jvm/suspend.rs build_state_machine`), but five frontend gaps gate the helper:
  (1) the `kotlin.coroutines.Continuation(ctx) {…}` factory function isn't resolved; (2) `startCoroutine`
  (extension on a `suspend () -> T`, seen as `Function`) isn't resolved; (3) a generic type param `T` is
  out of scope inside an anonymous `object : Continuation<T>`; (4) `override val context = …` property
  type can't be inferred; (5) a function-typed param (`x: (T)->Unit`) invoked by name `x(...)` isn't
  resolved inside the anon object. NEXT for coroutines = land those five, then the helper compiles and
  the genuinely-supported suspend tests flip to OK.

- **Phase P63 — top-level `const val` visibility + cross-file const reads (gate 1726 → 1730, +4,
  FAIL=0).** Two bugs. (1) The parser dispatched top-level `val`/`var` through `parse_top_property`
  (not `_c`), so `is_const` was dropped — top-level `const val X = …` emitted a `private` field instead
  of kotlinc's `public static final` + `ConstantValue`. Threaded `const` through the dispatch. (2)
  Cross-file `const val` reads (`// FILE:` tests) routed through a `getX()` accessor that const fields
  don't have (`NoSuchMethodError`). Now `is_const` is carried in `syms.props` (`(Ty,bool,bool)`) and
  `syms.prop_facades` (`(String,Ty,bool,bool)`); a cross-file const read lowers to
  `IrExpr::ExternalStaticField` (a direct `getstatic` of the public field) rather than a
  `Callee::CrossFile` accessor call. Matches kotlinc: const reads are field accesses, not getters.

- **Phase P62 — interface delegation through a non-`val` constructor parameter (gate 1723 → 1726, +3,
  FAIL=0).** `class C(a: I) : I by a` where `a` is a NON-`val` param had no backing field, so the forwarder
  (which looks the delegate up as a field) bailed. Now ir_lower synthesizes a `private final $$delegate_<i>`
  field per such delegation (kotlinc's name), the ctor stores the param into it (first in the body, after
  `super()`), and `synth_delegation_forwarders` routes each interface method through it. Handles multiple
  delegations (`A by x, B by y`). The long-standing `val`-param path is untouched. For the non-`val` path
  ONLY, two shapes still bail (skip, never miscompile): an interface with PROPERTIES (un-forwarded
  accessors → `AbstractMethodError`) and a GENERIC interface (`A<Long,Int>` needs substituted-type bridges).
  e2e `interface_delegation_e2e`.

- **Phase P61 — visibility-only property setter (`var x = 0; private set`) (gate 1705 → 1723, +18, FAIL=0).**
  A property with a visibility-only setter (no body — `private`/`protected`/`internal set`) bailed because
  `is_plain_body_prop` required `setter.is_none()`. It's still a plain backing-field property; only the
  setter's access flag differs. `is_plain_body_prop` now allows a body-less setter; the synthesized `setX`
  for a `private set` is recorded in a new `IrFile.private_methods` set and emitted `private final` (mirrors
  `open_methods`). e2e `private_set_e2e`. +18 box files (all visibility-only setters).

- **Phase P60 — inferred-type computed property (`val xx get() = x`) (gate 1702 → 1705, +3, FAIL=0).** A
  computed property without an explicit type annotation (the type is inferred from the getter body) bailed —
  `is_computed_prop` required `p.ty.is_some()` (else the type derivation `info.ty(p.init.unwrap())` panicked
  on `init == None`). New `body_prop_ty` helper derives the type from the annotation, else the getter body,
  else the initializer; `is_computed_prop` drops the annotation requirement. Both lowering sites (facade
  static + class-instance getter) use it. Covers a plain class and a value-class member (`@JvmInline value
  class Z(val x: Int) { val xx get() = x }`). e2e `inferred_computed_prop_e2e`.

- **Phase P59 — `var` generic delegated property: box the value into `setValue`'s erased param (gate 1691 →
  1702, +11, FAIL=0).** A generic delegate's `setValue(…, i: T)` takes the ERASED `Object`; a `var Int by
  Del(…)` previously bailed because the primitive value wasn't boxed before the call (VerifyError). The setter
  now boxes a primitive property value into `setValue`'s reference param via `ImplicitCoercion` (`Integer.
  valueOf`), exactly as kotlinc emits; the P58 read-side coercion handles `getValue`. Removes the var-bail
  guard. e2e `generic_delegate_e2e::generic_delegate_var_primitive_property`. +11 delegated-property box files.

- **Phase P58 — generic delegated member property: coerce `getValue`'s erased return (gate 1690 → 1691, +1,
  FAIL=0).** A generic delegate (`class Del<T> { operator fun getValue(…): T }`) returns the ERASED `Object`;
  a delegated member property `val s: String by Del(…)` previously bailed (the lowering guard rejected
  `getValue.ret != property type`). Now the getter coerces the `getValue` result to the property type via the
  existing `coerce_erased` (a `checkcast` for a reference property, unbox for a primitive) — exactly kotlinc's
  emit. Guard relaxed to allow an erased-REFERENCE return (still bails on a concrete mismatch). A `var`'s
  `setValue` whose value param erased to a reference while the property is a PRIMITIVE still bails (the value
  would need boxing first — the read-only half lands now). e2e `generic_delegate_e2e`.
- **Phase P57 — unsigned (`UInt`/`ULong`) value-class extension resolution via `@Metadata` (gate 1616 → 1690,
  +74, FAIL=0).** A value-class extension (`UInt.coerceAtMost`/`coerceIn`/…) has a `@JvmName`-MANGLED bytecode
  name (`coerceAtMost-J1ME1BU`) in a multifile PART class, indexed under the ERASED underlying descriptor —
  the literal-name lookup misses it, and `UInt`'s erased descriptor `"I"` makes the SIGNED `Int` extension
  shadow it. Four pieces, all reusing the Result machinery: (1) `functions()` resolves the mangled extension
  for a value-class receiver via `package_functions` (facade PARTS merged from the facade's `@Metadata` `d1`),
  matching the Kotlin name + `@Metadata` extension receiver, then loads the real candidate by the mangled JVM
  name; (2) the plain-extension loop now REJECTS a candidate whose `@Metadata` receivers are concrete and
  exclude this value class (not it, nor a supertype via `kotlin_subtype`) — so signed `Int.coerceAtMost` no
  longer binds a `UInt`; (3) the arg-matcher accepts a value-class argument (`3u`) for its erased-underlying
  param (`coerceAtMost-<hash>(II)`); (4) the logical return is recovered from `@Metadata` by the mangled JVM
  name (`MetaFn.ret_class`, new) so `b: UInt`, not `Int` (the by-Kotlin-name lookup is ambiguous across the
  4 unsigned overloads). e2e `unsigned_ext_e2e` (`coerceAtMost`, `coerceIn`). +74 unsigned-cluster box files,
  all run correctly under `-Xverify:all`.
- **Phase P56 — full `kotlin.Result` end-to-end: construction + extension + erasure, byte-equal (gate 1612 →
  1616, +4, FAIL=0).** `Result.success(42)` then `getOrThrow()` now compiles, runs under `-Xverify:all`, and
  is byte-equal to kotlinc. Pieces: (1) the checker resolves a value-class COMPANION call (`Result.success`)
  via a new `LibrarySet::value_companion_fn` (metadata: `class_companion_name` + `class_functions`, the
  companion fn is bytecode-private + public-inline), recorded in `TypeInfo.companion_calls`; (2) lowering
  emits the companion `getstatic <class>.Companion` receiver + an inline `Callee::Static` with
  `dispatch_receiver`; (3) emit's `try_inline_static_as` splices an INSTANCE inline method — the real
  descriptor fetches the body, a receiver-prepended `splice_desc` maps `this`=local0/params=local1.., and the
  splicer drops the unused `this` (`pop`) + inlines the single-use arg, exactly like kotlinc. Three
  kotlinc-faithful value-class rules completed the byte-equality: RETURN mangling applies to a `Result`-
  returning member (`C.foo(): Result` → `foo-d1pmJ48`, kotlinc `hasMangledReturnType`) but NOT a file-class
  (top-level) fn, while PARAM mangling stays exempt for `Result`; an external value class's bridge returns
  the underlying directly (no `box-impl`); and `as Result`/`is Result` erase to the underlying (no
  `checkcast Result`). `result_e2e` is green (un-`#[ignore]`d); `inlineClasses/returnResult/class*Override`
  box files pass.
- **Phase P55 — classpath value-class type erasure (`Result`→`Object`): the value-class pass now unboxes
  classpath value classes (gate 1611 → 1612, +1, FAIL=0).** krusty's unboxed value-class ABI pass
  (`jvm/value_classes.rs`) erased only USER value classes; a CLASSPATH value class typed in the file
  (`fun f(r: Result<Int>)`) kept the boxed `Lkotlin/Result;` form, diverging from kotlinc's erased
  `Ljava/lang/Object;`. Now ir_lower discovers every classpath value class referenced by type and records its
  REFERENCE underlying (`Result`→`Any`; a primitive-underlying `UInt`/`ULong` is EXCLUDED, keeping its
  dedicated handling) into a new `IrFile.external_value_classes` map (via `LibraryType.value_underlying`,
  populated from `class_inline`). The pass merges these into its erasure map so their types erase exactly like
  a user value class. Two kotlinc-faithful rules added: (1) `kotlin.Result` is EXEMPT from name mangling
  (kotlinc's `IrType.getRequiresMangling` is `!isClassWithFqName(RESULT_FQ_NAME) && …`) — `f(Result)` keeps
  the plain name `f`, not `f-<hash>`; (2) a classpath value class is only ever held UNBOXED here, so box/unbox
  at a boundary is identity (krusty never materializes its `box-impl` object). Verified byte-for-byte vs
  kotlinc (`bytediff` on `f(r)=r.getOrThrow()`: erased param + spliced body), and the gate gains a real
  Result-using box file. Construction (`Result.success`, a Companion instance-inline splice) is the last piece
  for the full `result_e2e`.
- **Phase P54 — metadata-primary resolution of an `inline` value-class extension (`Result.getOrThrow`) (gate
  1611 → 1611, +0, FAIL=0).** An `inline` extension on a value class is PRIVATE in bytecode (so the
  literal-name `find_extensions` finds it only at the receiver's erased `Object`/underlying rung, then
  rejects it for non-public visibility), but PUBLIC per `@Metadata`. `functions()` now, for a BYTECODE-private
  extension candidate only (the public ones already resolve, untouched — keeps the 1611 intact), consults
  `package_functions`: if the candidate is metadata-public + `inline` and its `@Metadata` extension receiver
  is EXACTLY this value class, it resolves as public (with `must_inline` still keyed on the bytecode
  visibility — no legal `invokestatic`, so the body is spliced). The metadata receiver disambiguates the
  erased-`Object` rung so `getOrThrow` binds only a `Result`, never an unrelated receiver. Verified by
  `metadata_reader_e2e`: `resolve_callable("getOrThrow", Result)` → `kotlin/ResultKt.getOrThrow` inline;
  `resolve_callable("getOrThrow", String)` → none. The body splices correctly (`throwOnFailure`; unbox).
  Byte-equal codegen for a `Result`-typed value additionally needs classpath value-class param/local erasure
  (`Result`→`Object`, layer 4) and the companion inline-splice for construction (`Result.success`); the
  target `result_e2e` stays `#[ignore]`d for those.
- **Phase P53 — metadata reader for classpath `@JvmInline value class` detection (gate 1611 → 1611, +0,
  FAIL=0).** Layer 2 prerequisite for `kotlin.Result`: krusty's unboxed value-class ABI pass
  (`jvm/value_classes.rs`) already lowers USER value classes, but had no way to recognize a CLASSPATH type as
  a value class or learn its underlying type. New `metadata.rs` `class_inline(ci) -> Option<InlineClass {
  underlying_class, property_name }>`, reading the `Class.inline_class_underlying_type`(=18) / `_property_name`
  (=17) / `_type_id`(=19) proto fields (presence is the value-class marker). `metadata_reader_e2e` validates:
  `Result` → underlying `kotlin/Any` (`value class Result<T>(val value: Any?)`, erases to Object), `UInt` →
  `kotlin/Int`, `Pair` → not a value class. Reader only (not yet consumed by the value-class erasure pass).
- **Phase P52 — metadata-primary function reader: signatures from `@Metadata`, bytecode is fallback (gate
  1611 → 1611, +0, FAIL=0).** Foundation for `kotlin.Result` (and every `inline` stdlib member). An `inline`
  function is `private`/synthetic in bytecode, so its *public* signature exists only in `@Metadata` — krusty
  built `companion`/members from the bytecode method table and never saw `Result.Companion.success`/`failure`
  or the `ResultKt` extensions (`getOrThrow`, …). Verified kotlinc's model in `JvmProtoBufUtil.
  getJvmMethodSignature`: name/params/return/visibility/`inline`/receiver come from the proto `Function`/
  `Class`/`Package` messages; the `method_signature` extension only *overrides* the JVM descriptor, else it's
  computed from proto types. New `metadata.rs` reader: `class_functions`/`package_functions` →
  `Vec<MetaFn{kotlin_name, jvm_name, jvm_desc, is_public, is_inline, is_suspend, receiver_class}>` (visibility
  decoded from `Flags`), and `class_companion_name`. When metadata omits the JVM descriptor (no `@JvmName`
  mangling), the bytecode method of that name is the fallback — covers value-class members erased to
  `(Object)Object` (`success`). `metadata_reader_e2e` validates against the real stdlib: `Result.Companion.
  success` is public+inline with desc `(Ljava/lang/Object;)Ljava/lang/Object;`; `ResultKt.getOrThrow` is a
  public inline extension on `kotlin/Result`. Reader only so far (not yet wired into resolution) — the two
  remaining Result layers are the inline-class unboxed ABI and inline-fn splicing of these bodies (target
  e2e `result_e2e`, `#[ignore]`d with that reason).
- **Phase P51 — wildcard imports `import a.b.*` were silently dropped (gate 1611 → 1611, +0, FAIL=0).**
  `parse_qualified_name` only keeps a path segment when an `Ident` follows the `.`, so for `import
  kotlin.coroutines.*` it consumed `kotlin.coroutines` and left the cursor on `*`, which the import
  parser's trailing-token tolerance loop then swallowed — the import was recorded as `kotlin.coroutines`
  (a bogus explicit import of a type named `coroutines`) and NEVER as a wildcard. So no non-default
  wildcard import (`kotlin.coroutines.*`, `kotlin.math.*`, `kotlin.reflect.*`, …) ever fed
  `import_wildcards`, and bare names from those packages were unresolvable. Fix: after
  `parse_qualified_name`, if the cursor is on `*`, consume it and record the import as `a.b.*` (the form
  `import_wildcards` recognizes; `import_map` already skips `.*`). e2e `classpath_object_via_wildcard_import`
  resolves `EmptyCoroutineContext` through `import kotlin.coroutines.*`. Gate count is unchanged because the
  unblocked files hit further blockers (coroutine helpers `EmptyContinuation`/`resume`, `kotlin/Result`
  members), but the survey confirms real progress: the `EmptyCoroutineContext` skip category (26) is gone and
  `getOrThrow on kotlin/Result` rose 36 → 59 as files now advance to their next real blocker.
- **Phase P50 — classpath `object` referenced as a value + kind-flag enums (gate 1611 → 1611, +0, FAIL=0).**
  Coroutine-chain layer 1b: a bare reference to a CLASSPATH Kotlin `object` (e.g. `EmptyCoroutineContext`)
  was an "unresolved reference". Now the checker's bare-`Name` fallback resolves the name through the generic
  import machinery (`imported_type_internal` — explicit imports + Kotlin default-import packages), and if the
  resolved library type is an `object` (`LibraryType::is_object()` — detected via a `public static final
  INSTANCE` field of the type's own descriptor), it types the reference as that object and records it; lowering
  emits a new `IrExpr::ExternalStaticField { owner, name: "INSTANCE", descriptor }` → `getstatic
  <owner>.INSTANCE`. e2e `classpath_object_value_e2e` round-trips `EmptyCoroutineContext.toString()` under
  `-Xverify:all`. Plus a design cleanup the maintainer flagged on review: the parallel kind booleans
  (`is_interface`/`is_object`/`is_enum`/`is_annotation`) on `ClassDecl` and (`is_interface`/`is_annotation`/
  `is_object`) on `LibraryType` are now a single `kind:` field — `ast::ClassKind` and `libraries::TypeKind`
  enums — read through `is_*()` accessor methods. `TypeKind::is_interface()` returns `true` for `Annotation`
  too (JVM annotations carry `ACC_INTERFACE`); the AST `ClassKind` keeps `Annotation` distinct from
  `Interface` (matching the parser, which never set `is_interface` on annotation classes). No behavior change
  — pure single-source-of-truth refactor; full suite + gate green.
- **Phase P49 — coroutine stdlib type resolution: `kotlin.*` wins ambiguous simple names + default-import
  packages in the generic import machinery (gate 1610 → 1611, +1, FAIL=0).** `kotlin.coroutines.Continuation`
  and `kotlin.Result` (and `CoroutineContext`) didn't resolve as types — even fully-qualified — because the
  classpath simple-name index PRUNES ambiguous names, and Java 25's jimage adds `jdk/internal/vm/Continuation`,
  `com/sun/.../Continuation`, plus several `Result` classes → "Continuation"/"Result" pruned. Both ARE in
  the stdlib jar (verified). Two fixes, both mirroring kotlinc's resolution model (a bare name resolves
  against default-import packages + imports, NOT every classpath class): (1) the type index now PREFERS a
  `kotlin/*` type over a non-kotlin one on a simple-name clash (kotlinc default-imports `kotlin.*`, so the
  kotlin type wins its bare name) — fixes the signature-collection resolver `ty_of_ref`; (2) the generic
  import machinery (`imported_type_internal`, used by `check_file`'s `resolve_ty`) now also consults
  Kotlin's fixed default-import package list + the file's wildcard imports, verifying existence via the
  federated `resolve_type` — no global-index reliance. This unblocks coroutine-stdlib TYPE resolution (the
  first of the coroutine-cluster chain; `Result.success` companion + helper-source injection + `Continuation`
  impl remain). FOLLOW-UP (owner-directed): retire the index `kotlin/*`-precedence patch by making
  `ty_of_ref` use the same generic import machinery (default imports + wildcards), so NO global every-class
  simple-name index is consulted — the fully faithful model.
- **Phase P48 — member property-read inference via federated resolution (gate 1610 → 1610, +0, FAIL=0).**
  Completes P46: `infer_lit_ty_p`'s property-read arm (`s.length`, `list.size`, `vc.value`) now resolves
  through the FEDERATED source too — `String`/`CharSequence` members via the source's builtin-member API
  (`builtin_member_ret`), object properties via the shared `call_resolver::resolve_instance` trying the
  property name, its `getX` accessor (new shared `property_getter_name`), and any mapped collection
  accessor — exactly the full checker's property-read path. NO hardcoded property names. +0 corpus (the
  member-expr-body property reads in the corpus carry other blockers), but it removes the last
  name-matching temptation from member inference (calls AND properties now federated) and is regression-
  free (full `just test` green). Foundation for the eventual `infer_lit_ty_p` retirement once the
  SymbolSource redesign federates the module's own declarations.
- **Phase P47 — faithful K2 backend-mute directive semantics (gate 1610 → 1610, +0, FAIL=0).** krusty
  targets Kotlin 2.4.0 = the **K2 frontend** + JVM_IR backend, so the conformance harness mutes tests as
  kotlinc's K2 runner does: honor `// IGNORE_BACKEND` (all frontends), `// IGNORE_BACKEND_K2
  [_MULTI_MODULE]`, and `// DONT_TARGET_EXACT_BACKEND` for JVM_IR — but NOT `// IGNORE_BACKEND_K1` (mutes
  only the OLD K1 frontend). Previously `IGNORE_BACKEND_K1` was wrongly excluded, under-counting: those
  ~270 tests were marked not-applicable when they ARE in scope for krusty's K2 semantics. Now attempted;
  all currently skip as unsupported (none miscompile — FAIL stays 0), so OK is unchanged but the harness
  faithfully matches kotlinc's backend applicability. Shared `conformance::backend_applicable` keeps the
  gate + survey in lockstep.
- **Phase P46 — member return-type inference via federated resolution (no hardcoded names); shared
  conformance directives (gate 1592 → 1610, +18, FAIL=0).** Two related fixes:
  (1) `infer_lit_ty_p` (the signature-collection pre-pass that infers an expression-bodied member's
  return type) name-matched stdlib symbols (`toString`, `shl`/`or`/`xor`, `toLong`, …) — prohibited
  hardcoding. Now it resolves method/extension/function return types through the FEDERATED `SymbolSource`
  (`src.functions(name, receiver)`, the same classpath/stdlib resolution the full checker uses), so
  `s.uppercase()`, `x.toString()`, library members type with ZERO hardcoded names. Genuine primitive
  INTRINSICS (the named bitwise operators `shl`/`and`/…, numeric/char conversions) — which compile to JVM
  opcodes, not classpath methods, so they're absent from `functions()` — go through the SHARED helpers
  the checker also uses (`conversion_target`, and a new `builtin_bitwise_ret` extracted so the checker and
  the pre-pass share ONE list, not two). Deleted the duplicated `prim_conversion_ret`. +14 corpus.
  (2) Gate/survey directive drift: extracted `krusty::conformance` as the SINGLE source of truth for
  backend applicability (`TARGET_BACKEND`/`IGNORE_BACKEND*`/`DONT_TARGET_EXACT_BACKEND` — the last newly
  honored, matching kotlinc's runner, which excludes the JVM_IR backend krusty emits, e.g.
  `@EagerInitialization`) and the per-test EXTRA-library set (`extra_libs`). The gate, the `survey` bin,
  and `tests/common` classpath now all consult it, so survey no longer over-counts by compiling against
  libraries a test didn't request.
- **Phase P45 — local classes (slice 2b: named local objects) (gate 1592 → 1592, +0 corpus, FAIL=0).**
  Completes the local-type-decl surface: a NAMED local object (`object Counter { … }`) now parses + hoists
  like the other local types. Distinguished from an anonymous-object EXPRESSION (`object { … }` /
  `object : T { … }`, which stays on the expr path) by the token after `object` being a name. +0 on the
  corpus (no box file is blocked solely on a named local object), but it removes a real "not parsed" gap
  and is proven by `named_local_object` e2e (singleton members + an anonymous object alongside, no
  regression). Local-class surface now: class / data class / interface / object / inheritance — only the
  CAPTURING case remains (outer locals → synthetic ctor fields, needs type-info-driven capture typing).
- **Phase P44 — local classes (slice 2a: modifier-prefixed + inheritance) (gate 1590 → 1592, +2,
  FAIL=0).** Extends P43: a local class may now carry `open`/`abstract`/`private`/… modifiers, enabling
  local-class inheritance (`open class Base; class Derived : Base()`). The parser's local-type lookahead
  scans through declaration modifiers to the `class`/`interface` keyword; the statement arm consumes the
  modifier/annotation prefix (`skip_decl_prefix`, as the top-level path does) and applies `open`/
  `abstract` to the parsed decl before hoisting. Still non-capturing only: a modifier-prefixed local
  class that reads an outer local fails to resolve → file skips (sound). New e2e case
  `local_class_inheritance_with_modifiers` (open override + virtual dispatch through the base type,
  abstract + impl).
- **Phase P43 — local classes (slice 1: non-capturing) (gate 1582 → 1590, +8, FAIL=0).** A `class`/
  `data class`/`interface` declared inside a function body was unparsed (`class` in a block → "expected
  an expression"). Now parsed as `Stmt::LocalClass(ClassDecl)` (parser detects a local type decl via
  lookahead — `class`/soft-keyword `data`/`enum`/`sealed`/`annotation`/`value` + `class`, or
  `interface Name`) and HOISTED post-parse (`hoist_local_classes`) to a top-level-equivalent
  `Decl::Class`, so signature collection, checking, and lowering treat it like any other class — the
  in-body `Stmt::LocalClass` is a no-op. A CAPTURING local class is checked with no enclosing scope, so
  its outer references fail to resolve and the file cleanly skips (never miscompiles). SOUND SLICE
  BOUNDARY: a local class with super-constructor ARGS (`class Z : C(s)`) isn't hoisted (its base-arg
  capture isn't rejected by the outer-scope-free check → would miscompile, VerifyError); modifier-
  prefixed (`open`/`abstract`) local classes stay on the expr path (skip) — both are slice 2. Also fixed
  a latent scope leak: `check_file` now resets to the base scope depth before each top-level decl (a
  prior function's locals must not be visible to a hoisted class). New `local_class_e2e` (fields/methods,
  `data class` equality, local `interface` + impl).
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
- **Phase P23 — parallelize the full test suite (test-time).** `just test` built once then ran the
  JVM-bound e2e binaries in parallel (`xargs -P $(nproc)`, each keeping its shared JVM); the rayon
  conformance gate ran first/alone to avoid contention. The suite has since grown/regressed; current
  agents should use `./run-tests.sh` / `just test` and the printed slowest-binaries table as the live
  profiling source. Failure-aware (any binary's non-zero exit fails the run + prints its log); a filter
  arg defers to plain `cargo test --profile gate`. `nextest` was worse (82s).
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
- **Casts to type parameters + nullable references.** `x as T` (type-param unchecked cast) and `x as
  Foo?` (nullable reference cast) now lower instead of skipping the whole file. `T` is kept with its
  bound in the IR (`IrType::TypeParameter`) and erased ONLY at JVM emit (the "no erasure in the checker"
  rule); a non-null bound null-checks, an unbounded `T` emits no `checkcast`. A nullable reference cast
  is a `null`-passing `checkcast` (its non-null form names the class); a nullable value-class target
  skips (would unbox `null`). Unmodeled coercions (`<Unit>`/`<Nothing>` results, erased generic calls
  inside `inline`, the `genericSafeCasts` flag) skip, never miscompile. Gate +8 box()=OK across the two,
  0 miscompiles. `tests/typeparam_cast_e2e.rs`, `tests/nullable_cast_e2e.rs`.
- **Named constructor arguments (`C(b = 9)`).** The checker now maps named ctor args to the primary
  ctor's parameter names (read from the same-file `ClassDecl`, reusing the function path's
  `map_call_args`); the lowering was already capable. A regression caught by review + fixed before
  landing: a named call must target the PRIMARY ctor only — the secondary-constructor selection in
  lowering is now gated on the call being positional, else `C(b = 9)` against a same-arity
  `constructor(x: Int) : this(x, x)` set `a` from the secondary (wrong fields). `tests/named_ctor_args_e2e.rs`.
- **Survey roadmap diagnostic.** `lower_file` records WHY it last skipped a file
  (`ir_lower::lower_bail_reason`: phase tags `gate:class`/`deep:fun` plus the innermost `expr
  Lambda`/`call Foo`), surfaced by the `survey` binary so the "lower bailed" bucket is an actionable
  roadmap rather than one opaque category. Read only by `survey`; zero compiler behaviour. `just survey
  [CATEGORY]` runs it against the version-matched cached corpus.
- **Primitive→reference box cast (`42 as Any`, `'a' as Char?`, `b as Byte?`).** A primitive operand
  cast to a reference type now boxes (its wrapper is-a the target) instead of the checker rejecting it.
  Two guards keep it sound (review-driven): (1) a type-parameter target (`56 as T`) is excluded — an
  initial version without it caused 5 miscompiles (boxing into an erased/bridged generic slot →
  VerifyError/CCE); (2) an *impossible* cast (`1 as String`) is rejected, not boxed (the wrapper must
  be assignable to the target via `obj_is_subtype`) — boxing `Integer` into a `String` slot would be a
  load-time VerifyError. Gate 1763/0. `tests/primitive_box_cast_e2e.rs`.
- **Property with a backing field + custom accessor (`val x = init get() = field…`).** A property with
  BOTH a stored field and a custom getter/setter referencing `field` now lowers (was a `gate:class`
  skip). The field is emitted; the synthesized `getX`/`setX` run the custom accessor body with `field`
  bound to the backing field. The hard part, surfaced by TWO review rounds: EVERY access to such a
  property — even in-class `x` / `x = …` / `x += …` / `x++` — must route through `getX`/`setX`, never
  the raw field (krusty's direct-field optimization is correct only for default accessors). `resolve_field`
  plus the unqualified read/write/incdec sites all decline a custom-accessor property (tracked in
  `field_accessor_props`); only the `field` keyword reaches the field. Two miscompiles (internal
  read/write, then `x++`) were caught by review and fixed before landing. Gate 1767/0.
  `tests/backing_field_accessor_e2e.rs`.
- **Interface delegation to an expression (`class D : I by Impl()`).** Was a parser hard error ("only
  `by <val-parameter>`"). Now the parser captures the delegate expression (`delegation_exprs`), the
  checker types it in the CTOR scope (so `by mk(x)` referencing a ctor param resolves), and the
  lowering evaluates it once into a `$$delegate_e<j>` field + forwards the interface methods (the
  forwarder was refactored to `forward_iface_methods`, shared with the `val`-param path). Review caught
  two correctness regressions (delegate typed in the wrong scope → spurious errors; `parse_expr`
  swallowing the class-body `{ }` → added a `no_trailing_lambda` parser flag) and the gate caught two
  miscompiles (a VALUE-class delegate is unboxed and doesn't implement `I` → skip; a `.`-containing
  filename emitted an illegal facade name → `file_class_name` now sanitizes). Gate 1767 → 1773 box()=OK,
  0 miscompiles. `tests/interface_delegation_expr_e2e.rs`.
- **Enum class generic `Signature` attribute (byte-parity FIX, gate box()=OK flat, FAIL=0).** Every
  `enum class E` extends the generic `java.lang.Enum<E>`, so kotlinc emits a class
  `Signature: Ljava/lang/Enum<LE;>;` (plus a raw `L<itf>;` per superinterface). `emit_enum_class` named
  only the erased `java/lang/Enum` superclass and emitted NO `Signature`, so `javap` showed `extends
  java.lang.Enum` instead of `extends java.lang.Enum<E>` — a per-enum divergence on EVERY enum, surfaced
  by `bytediff when/enumOptimization` (`En`, `ABCD`, `Season`, `E`, …). Now `emit_enum_class` sets
  `Ljava/lang/Enum<LSelf;>;` (+ interfaces). Runtime-inert (the JVM ignores `Signature` for execution) —
  box()=OK unchanged, FAIL 0. `tests/enum_class_signature_e2e.rs`. Separate remaining enum-class diffs
  (out of scope): the `$EnumSwitchMapping$` `when`-over-enum tableswitch optimization; a generic class
  bound `<T : Enum<T>>`.
- **Enum `entries` members + `$values()` helper + member order (byte-parity FIX, box()=OK flat, FAIL=0).**
  Kotlin 2.x emits, on EVERY enum: a `private static final kotlin/enums/EnumEntries $ENTRIES` field
  (the `entries` backing), a `public static EnumEntries<E> getEntries()` accessor (generic
  `Signature ()Lkotlin/enums/EnumEntries<LE;>;`), and a private synthetic `$values()` array builder that
  `<clinit>` calls; `<clinit>` then does `$ENTRIES = EnumEntriesKt.enumEntries((Enum[]) $VALUES)`.
  Member order is `<init>, values, valueOf, getEntries, [user methods], $values, <clinit>`. krusty
  emitted none of these and inlined the array build directly in `<clinit>` (which it emitted early), so
  every enum diverged. `emit_enum_class` now adds the `$ENTRIES` field, builds `<clinit>` but ADDS it
  LAST, emits `getEntries`/`$values`, and routes `<clinit>` through `$values()` + the `$ENTRIES` init.
  Runtime-safe (the `<clinit>` `enumEntries` call resolves against stdlib): box()=OK unchanged, FAIL 0.
  `tests/enum_entries_e2e.rs`.
- **A plain `enum class` is now BYTE-IDENTICAL to kotlinc (box()=OK flat, FAIL=0).** Four remaining
  per-enum divergences fixed together; `bytediff` on `enum class Color { RED, GREEN, BLUE }` reports
  100% byte-identical (both `Color` and the `ColorKt` facade):
  1. `e.ordinal`/`e.name` dispatched on the receiver's STATIC type (`invokevirtual Color.ordinal`), not
     `java/lang/Enum.ordinal` — fixed in `ir_lower` (a `Callee::Virtual` with `owner = ci.fq_name`
     instead of `Callee::External("java/lang/Enum.ordinal")`). General invokevirtual-owner rule.
  2. The enum ctor is `private` (`ACC_PRIVATE`), not `ACC_SYNTHETIC` — kotlinc keeps it private even
     when entry subclasses call it (via nestmates; see below).
  3. The enum ctor carries a generic `Signature` listing only the USER params (`()V` for a plain enum,
     `(I)V` for `E(val n: Int)`) — javap reads it to hide the synthetic `(String,int)`; without it the
     synthetic params leaked into the disassembly.
  4. `values()` invokes `clone()` via `java/lang/Object` (kotlinc), not the `[LE;` array type.
  `tests/enum_entries_e2e.rs` (`plain_enum_ctor_is_private_nonsynthetic_with_signature`). Remaining
  enum diffs (separate, larger): a SUBCLASSED enum (bodied entries) — kotlinc keeps the base ctor
  `private` and relies on `NestHost`/`NestMembers` (krusty emits neither, so it stays package-private),
  and the entry-subclass `E$A` ctor needs the same `()V` Signature (separate emit path); the
  `$EnumSwitchMapping$` `when`-over-enum tableswitch optimization.
- **`StringBuilder.append(String)` for a String from a method-return descriptor (byte-parity FIX,
  box()=OK flat, FAIL=0).** A String value whose `Ty` was parsed from a method RETURN descriptor —
  a classpath call (`"ab".uppercase()`) or the data-class `Arrays.toString(field)` wrapper — is
  `Ty::Obj("java/lang/String")`, NOT `Ty::String`. `append_top` matched only `Ty::String`, so it
  appended such values via the less-specific `append(Ljava/lang/Object;)` where kotlinc uses
  `append(Ljava/lang/String;)` — a per-concat divergence (every string template/`+`/data-class
  `toString` over a String-returning call). `append_top` now treats `Ty::Obj("java/lang/String")` /
  `"kotlin/String"` as `String`. (A plain String field read was already `Ty::String` via `ir_ty_to_jvm`,
  so only descriptor-typed values were affected.) `tests/string_concat_append_overload_e2e.rs`. Surfaced
  by `bytediff dataClasses/equals/intarray` (the IntArray data-class `toString`).
