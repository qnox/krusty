# krusty → kotlinc parity protocol

Goal (session directive): finish the Kotlin→JVM compiler rewrite in Rust as a **drop-in replacement for
kotlinc**. No compiler-plugin/extension support; every other compiler part must work. The produced
**bytecode must equal** the reference kotlinc's. Validate against the conformance tests in
`~/external-projects/kotlin`. Maintain our own test suite. Commit + push after each phase. Keep test
execution **< 60s** (profile/optimize otherwise). No hacks/workarounds/bails. TDD.

## Definitions of done

- **Runtime correctness**: `box()=="OK"` under `-Xverify:all` on the codegen/box corpus (the `kotlin`
  repo's `compiler/testData/codegen/box`). Current gate: **1361 OK / 0 FAIL** (scanned 7351).
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
  - Heavy tier (~234s): the full `cargo test` compiles 40 test binaries and runs 39 e2e suites that each
    spawn the real kotlinc/javac/java (the differential bytecode-parity validation the goal requires).
    Inherently > 60s — kotlinc JVM startup alone is ~1–2s × many. This is the CI/pre-push tier, not the
    iteration loop. (Profiled: conformance dev-profile run = 51s; gate-profile = 38s; the rest is e2e
    JVM spawns + test-binary compile.)
- kotlinc 2.4.0 runs on JRE 25 (verified). bytediff is slow (one kotlinc JVM launch per file) — sample.

## Phase log

(newest first — every entry = a committed+pushed phase, gate FAIL=0)

- **Phase P1 — for-in-local-array: no redundant array copy.** `lower_for_each` copied the iterable into
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
- _known next divergences (from `bytediff` over the corpus)_: array-literal init (`intArrayOf(…)` uses
  `dup`-per-element; kotlinc stores to a temp + `aload`-per-element); primitive-array `iterator()` loops
  (krusty ~24 bytes larger); more loop forms (ranges/downTo/step/indices) to audit for kotlinc's
  counted-loop optimizations.
