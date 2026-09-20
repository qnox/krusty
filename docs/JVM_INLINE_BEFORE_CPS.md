# JVM: splice inline bodies before the coroutine transform

Status: **design, not yet implemented.** Reviewable decision record for a large, staged change.
Scope: the JVM backend only. Other backends keep today's IR-level inline expansion and IR-level
coroutine lowering, unchanged.

## 1. The decision

Give the JVM backend kotlinc's ordering:

```
  today (krusty, JVM)                    proposed (JVM)                  other backends
  ───────────────────                    ──────────────                  ──────────────
  IR                                     IR                              IR
   → realize InlineBodyPlan (IR)          → lower_suspend_abi (IR)        → realize InlineBodyPlan
   → lower_suspend  (IR → IR CPS)         → ir_emit pass 1 (discovery)     → lower_suspend (IR → IR)
   → ir_emit                                 └ splice_unified (bytes)      → emit
      └ splice_unified (bytes)            → liveness over those bytes
                                          → ir_emit pass 2 (state machine)
```

The coroutine (CPS) state machine moves from an **IR → IR** pass that runs *before* inline
expansion to a **bytecode → bytecode** pass that runs *after* it. This is the same split kotlinc
uses: its IR lowering fixes the suspend ABI, and `CoroutineTransformerMethodVisitor` does the state
machine, the liveness analysis and the spilling on the already-inlined `MethodNode`. krusty reaches
the same ordering by emitting the body twice (§5.2) rather than by editing a finished method.

## 2. The symptom this exists to fix

`JVM backend inline error: call arity mismatch` — always exactly one operand short, and the missing
operand is always the `Continuation`.

```kotlin
class C {
    suspend fun one(v: String): String = v + "!"
    suspend fun many(xs: List<String>): Any? = xs.filter { one(it).isEmpty() }
}
```

A suspension inside a classpath `inline` function's lambda compiles today **only** where an
`InlineBodyPlan` exists (`src/libraries/inline_body.rs`: `InvokeLambda` / `Iteration` /
`CollectionTransform`), because those plans are realized into IR *before* `lower_suspend` runs.
Every other inline body reaches `splice_unified` (`src/jvm/inline.rs`) at **emit** time — after CPS
— so the spliced suspend call is emitted with no continuation to pass.

Measured 2026-09-20, a suspend call inside the lambda:

| compiles | bails |
| --- | --- |
| `map`, `flatMap`, `forEach`, `let`, `run`, `apply`, `withIndex().map` | `filter`, `mapNotNull`, `any`, `first`, `takeIf`, `onEach`, `sumOf`, `sortedBy`, `groupBy`, `associateWith`, `associateBy`, `mapValues`, `mapKeys` |

13 of 20 common shapes bail. `filter` with a **non**-suspend lambda body compiles and emits no
lambda class, so the splice itself is fine — only the suspension is unreachable.

Corpus impact (private harness, 28201 classes): three modules emit **nothing**, all on this single
bail — 522 + 241 + 224 ≈ 987 classes.

## 3. The decisive evidence: the spill set

`kotlinc 2.4.0 -d out C.kt`, then `javap -p -c -cp out C`. Output is `C.class` and `C$many$1.class`
— the continuation class. **No lambda class**: `filter` and its lambda are both fully inlined into
`many`.

Suspension point, with its spill block (offsets from the real disassembly):

```
   154: iconst_0
   155: istore        11                     // $i$a$-filter-C$many$1  (inline-depth marker)
   157: aload_0                              // receiver for one()
   158: aload         10                     // argument
   160: aload         13                     // continuation
   162: aload 13 ; aload_1 ; nullOutSpilledVariable ; putfield C$many$1.L$0
   171: aload 13 ; aload_3 ; nullOutSpilledVariable ; putfield C$many$1.L$1
   180: aload 13 ; aload 5 ; nullOutSpilledVariable ; putfield C$many$1.L$2
   190: aload 13 ; aload 6 ;                           putfield C$many$1.L$3
   197: aload 13 ; aload 8 ;                           putfield C$many$1.L$4
   204: aload 13 ; aload 9 ;                           putfield C$many$1.L$5
   211: aload 13 ; aload 10; nullOutSpilledVariable ; putfield C$many$1.L$6
   221: aload 13 ; iconst_1 ; putfield C$many$1.label
   227: invokevirtual one:(Ljava/lang/String;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;
   230: dup
   231: aload         14                     // COROUTINE_SUSPENDED
   233: if_acmpne     320                    // normal path: result already on the stack
   236: aload         14
   238: areturn
```

Seven references are spilled:

| field | local | what it is | exists before `filter` is inlined? |
| --- | --- | --- | --- |
| `L$0` | `aload_1` | the `List<String>` parameter `xs` | **yes** — source local |
| `L$1` | `aload_3` | `$this$filter`, the `Iterable` receiver | no |
| `L$2` | `aload 5` | `$this$filterTo`, the inner receiver | no |
| `L$3` | `aload 6` | `destination`, the fresh `Collection` | no |
| `L$4` | `aload 8` | the `Iterator` | no |
| `L$5` | `aload 9` | the raw `Object` element | no |
| `L$6` | `aload 10` | `it`, the `String` lambda parameter | no |

**Five of seven do not exist until `filter` has been inlined.** A pass that runs before the splice
cannot compute this set. This is not a matter of patching the current order harder; the information
is not there yet.

Two further facts the same listing settles:

* **Constant locals are rematerialized, not spilled.** Slots 4, 7 and 11 (`$i$f$filter`,
  `$i$f$filterTo`, `$i$a$-filter-…` — inline-depth markers, always `0`) are *not* in the spill set.
  The resume path re-establishes them with `iconst_0; istore` (offsets 239-246) before restoring
  `L$6 … L$0` in **reverse** field order. krusty already has the concept under a different name
  (`is_rematerialized_null` in `spill_layout.rs`).
* **Slot numbering.** Source and inline locals occupy 1..11; `$result` is 12, the continuation 13,
  `COROUTINE_SUSPENDED` 14. The machine's own locals are numbered **after** every source and inline
  local — which is only possible if the inline locals are already allocated when the machine is
  built. (krusty numbers `$continuation` before the source locals today; see
  `krusty-suspend-method-slot-and-lvt`.)
* Dispatch is a `tableswitch { 0: 88, 1: 239, default: 360 }`. krusty emits an `if_icmpne` chain.
  Runtime-equivalent, but not byte-equal; the new machine should emit the `tableswitch`.

Independent corroboration: the stdlib ships `$$forInline` copies of suspend inline functions — the
*untransformed* body kotlinc keeps precisely so that an inliner can consume it before the coroutine
transform has run.

## 4. Why the previous attempt stopped

Branch `feat/suspend-in-spliced-inline`, commit `2a360573` (WIP, not for merge). It kept the current
order and pushed the IR pass into the splice: the CPS analysis descended into `inline_body` lambdas,
the flattener rewrote such a suspension in place, the spill layout was extended, and
`IrExpr::SplicedResume` + `IrFile::spliced_suspension_sites` carried the resume arm's branch back
into the splice.

It stopped on three walls, each the same shape — *an IR pass depending on emit-time allocation*:

1. the lambda's locals live in the splicer's scratch frame, so the machine cannot spill them without
   rehoming them into the caller's value space;
2. the host inline body's own locals are invisible to the IR pass entirely;
3. the IR if-chain dispatch has to branch *into* bytecode through a late-bound label.

It also regressed a passing test —
`invoke_operator_extension_e2e::suspend_classpath_member_extension_is_a_suspension_point` — because
"carries an `inline_body`" is not a sound proxy for "this body runs in my frame".

Per-shape `InlineBodyPlan` decoders are likewise ruled out as a direction: the requirement is a
generic mechanism, and a general bytecode→IR translator is explicitly out of scope.

## 5. Target architecture

### 5.1 What stays in IR (`lower_suspend_abi`)

The part of today's `lower_suspend` that does **not** depend on the spill set:

* add the trailing `kotlin.coroutines.Continuation` parameter; erase the return type to
  `java.lang.Object`;
* thread the caller's continuation into each suspend call it can see at IR level;
* classify leaf (no suspension point) vs state-machine functions;
* declare the continuation class (`Facade$fn$1 extends ContinuationImpl`) with its `label`,
  `result`, and captured-receiver fields — **but not its spill fields**.

For a function whose machine moves to emit, `lower_suspend` stops there: it does not flatten the
body, does not allocate spill scopes, and does not synthesize the state machine. It records the
function as emit-time-machine and leaves the body alone.

Suspension sites need no in-stream marker instruction. Pass 1 (§5.2) records each suspension's byte
offset as it emits it, and pass 2 re-emits the body rather than editing it, so nothing has to travel
with relocated code. This is the one place krusty can be simpler than kotlinc, which must mark
because it edits a finished `MethodNode` in place.

### 5.2 What becomes a bytecode-informed pass: two-pass emission

The obvious realization — emit the method, then rewrite the finished `Code` attribute — was tried on
paper and rejected. Inserting the dispatch, the spill blocks and the resume blocks shifts every byte
offset, so every `StackMapTable` frame has to be re-derived; deriving them from bytecode alone means
writing a type-inferring verifier. That is a far larger and riskier component than the transform it
would serve.

Two-pass emission reaches the same ordering without it:

**Pass 1 — discovery.** Emit the suspend function's body into a scratch `CodeBuilder` with no state
machine at all: classpath inline bodies are spliced exactly as they are today, and each suspension
call is emitted as an ordinary call whose byte offset is recorded against its suspension index.
What comes out is precisely the post-splice bytecode kotlinc's `CoroutineTransformerMethodVisitor`
would see.

**Analysis.** Over those bytes: `disassemble` → `ControlGraph` → `LocalLiveness`
(`src/jvm/suspend/cps/`). For each recorded suspension, the locals live across it are the spill set —
computed over locals that only exist because the splice put them there. The pass also yields the
body's `max_locals`, which fixes where the machine's own slots go.

**Pass 2 — emission.** Emit the real method: the machine's slots (`$continuation`, `$result`,
`$suspended`) are placed at `max_locals..`, above every body and inline local, which is also how
kotlinc numbers them. The entry prologue, the `tableswitch` dispatch, the per-suspension spill block,
the `COROUTINE_SUSPENDED` check and the resume blocks are all emitted through the ordinary
`CodeBuilder` label and frame API, so frames are produced the way every other method's frames are
produced. The body emission itself is identical to pass 1, which is what makes pass 1's slot
numbering and spill sets valid for pass 2.

The continuation class is written after the method, because its spill fields and its
`@DebugMetadata` `l`/`n`/`s` vectors are products of the analysis.

The cost is emitting a suspend method body twice. That is paid only by suspend functions, only on
the JVM, and it buys the whole ordering.

### 5.3 The stack invariant

At the suspension, the call's own operands (receiver, arguments, continuation) are already pushed
when the spill block runs; the spill sequence is stack-neutral, so that is fine. What must hold is
that the operand stack *below* those operands is empty, because the resume block is a branch target
reached from the dispatch with an empty stack and has to reconstruct the join.

* In the `filter` shape the under-stack is empty (offset 154-155 leave nothing).
* In the `map` shape it is `[Collection]` — the destination is pushed before the lambda body. krusty
  already models this as `RelocatedLambdaSite.stack_prefix`.

**v1 (step 3) requires an empty under-stack and bails otherwise** — the bail is the status quo,
so this strictly cannot regress. **v2 (step 5) spills the under-stack into scratch locals first**,
as kotlinc does, and removes the restriction.

### 5.4 Non-JVM backends

Untouched. `InlineBodyPlan` realization and the IR coroutine lowering stay exactly where they are
for every other target; the JVM simply stops consuming them. The ≈5660 lines under
`src/jvm/jvm_libraries/inline_body_plan/` become deletable **for JVM** only after measurement shows
no regression, and only if no other consumer remains. That deletion is explicitly *not* part of this
work; it is a follow-up gated on numbers.

## 6. Staging

Each step is its own PR, rebased onto `origin/master` first, green on `./run-tests.sh`, with a test.

| # | Step | Gate |
| --- | --- | --- |
| 0 | This note. | docs only |
| 1 | The two analyses: control-flow graph and backward local liveness over decoded bytecode (`src/jvm/suspend/cps/`). | green |
| 2 | Pass 1 and the analysis wiring: emit a declined suspend body into a scratch builder, record each suspension's offset, and read its spill set. No emitted output changes yet. | green |
| 3 | Pass 2 — the state machine, for the **currently bailing** shapes only. The IR machine still owns every method it owns today, so the 11270 byte-identical classes cannot move. Fixture A (§7) lands here, asserting `box()` against the reference compiler. | green + fixture A runs |
| 4 | Continuation-class finalization: spill fields and `@DebugMetadata` become products of the bytecode pass for the new-machine methods. | green |
| 5 | Under-stack spilling (§5.3 v2). Fixture B lands here. | green + fixture B runs |
| 6 | Measure the corpus. Expect the three blocked modules to emit (≈987 classes). Report before/after. | corpus report |
| 7 | Migrate the remaining suspend methods onto the bytecode machine, one shape family at a time, each with a byte-parity delta. Emit the `tableswitch` and kotlinc's slot numbering here — both are byte-parity changes, not correctness ones. | per-step byte delta |
| 8 | Retire the IR machine for JVM; consider the `inline_body_plan` deletion. | measured |

Every fixture asserts the behaviour it is supposed to have, never the bail it currently gets:
`AGENTS.md` rule 9 rejects a diagnostics test that asserts only rejection. So a shape's fixture
lands in the step that makes it work, not before, and the "before" side of the comparison lives in
this note (§7) rather than in a characterization test.

Step 3 is the one that pays for the work; steps 1 and 2 exist so that step 3 is small.

## 7. Test strategy — no stdlib functions

Fixtures must **not** rely on stdlib inline functions. A stdlib `filter` proves nothing repeatable:
its body is whatever Kotlin distribution happens to be provisioned. Tests compile their own inline
library with the reference compiler and put it on krusty's classpath —
`tests/common::kotlinc_library` / `expect_box_run_against_kotlinc` already do exactly this, which is
what makes the inline function *classpath* (and therefore spliced) rather than same-file. Same-file
inline functions take a different code path (`krusty-two-inline-paths`), so the fixture must go
through a compiled library or it tests nothing.

### Fixture A — v1, empty operand stack under the suspension

```kotlin
// Lib.kt — compiled by the reference compiler onto the classpath
inline fun twice(x: Int, f: (Int) -> Int): Int {
    var acc = 0
    var i = 0
    while (i < 2) {
        val r = f(x + i)
        acc = acc + r
        i = i + 1
    }
    return acc
}
```

```kotlin
// Main.kt — compiled by krusty against that classpath
suspend fun one(v: Int): Int = v + 1
suspend fun many(v: Int): Int = twice(v) { one(it) }
```

krusty at `644f8d30` rejects `Main.kt` with `JVM backend inline error: call arity mismatch`
(measured). The reference compiler produces the full target shape — same
`tableswitch { 0: 88, 1: 218, default: … }` dispatch, same `dup; aload SUSPENDED; if_acmpne` normal
path — with five spilled locals:

```
    93: iload_0 ; istore_2          // slot 2 = x, the inline parameter copy
    95: iconst_0; istore_3          // slot 3 = $i$f$twice       (constant marker)
    97: iconst_0; istore 4          // slot 4 = acc
   100: iconst_0; istore 5          // slot 5 = i
   103: iload 5 ; iconst_2 ; if_icmpge 239
   109: iload_2 ; iload 5 ; iadd
   113: istore 6                    // slot 6 = the lambda argument
   115: iconst_0; istore 7          // slot 7 = $i$a$-twice-…    (constant marker)
   118: iload 6                     // ── operand stack under the call starts HERE: empty below
   120: aload 9                     // continuation
   122: aload 9 ; iload_0 ; putfield I$0      // v   (the source parameter)
   128: aload 9 ; iload_2 ; putfield I$1      // x
   134: aload 9 ; iload 4 ; putfield I$2      // acc
   141: aload 9 ; iload 5 ; putfield I$3      // i
   148: aload 9 ; iload 6 ; putfield I$4      // the lambda argument
   155: aload 9 ; iconst_1 ; putfield label
   161: invokestatic  one:(ILkotlin/coroutines/Continuation;)Ljava/lang/Object;
   164: dup ; aload 10 ; if_acmpne 218
   170: aload 10 ; areturn
```

| field | local | what it is | exists before `twice` is inlined? |
| --- | --- | --- | --- |
| `I$0` | slot 0 | the parameter `v` | **yes** — source local |
| `I$1` | slot 2 | `x`, the inline parameter | no |
| `I$2` | slot 4 | `acc` | no |
| `I$3` | slot 5 | `i` | no |
| `I$4` | slot 6 | the lambda argument | no |

Four of five are created by the splice — the same property that makes `L$1..L$6` impossible to
compute early, in four lines of library and two lines of main, with no stdlib function involved.
Slots 3 and 7 are constant inline-depth markers and are rematerialized on resume (`iconst_0;
istore`) rather than spilled, exactly as in the `filter` case.

### Fixture B — v2, a live operand stack under the suspension

Identical, except that the lambda result is consumed in place rather than stored first:

```kotlin
        acc = acc + f(x + i)
```

`acc` is then on the operand stack when the suspension is reached. The reference compiler spills it
into a scratch local before the call (`istore 8`) and reloads it after (`iload 8`), which adds a
sixth field `I$5`. krusty at `644f8d30` rejects this one with the same
`call arity mismatch` (measured). It lands in step 5, once §5.3's v2 under-stack spilling exists.

Correctness is `box()` against the reference compiler, not a byte diff; byte parity is a separate,
later measurement (step 7).

## 8. Risks

* **Liveness on bytecode is a new analysis.** Getting it wrong is a miscompile, not a bail. Mitigated
  by step 1: the analysis lands as an identity transform and is exercised over every suspend method
  in the gate before it is allowed to change anything.
* **Two machines coexist for steps 3-7.** Accepted deliberately: it is what keeps the existing
  byte-identical classes from moving while the new machine is unproven. The cost is a gate that must
  keep both green; the alternative is a single flag day over ~6000 lines of behaviour.
* **`max_stack`/`max_locals` and frame re-binding** are easy to get subtly wrong and fail only at
  class-load time. The gate runs `box()` on a real JVM, which catches it; `VerifyError` is the
  expected failure mode, not silent divergence.
* **The marker contract** must survive relocation into an unrelated constant pool. It is an
  `invokestatic` to a synthetic name, which `relocate_insns` already handles like any other call.

## 9. Measuring

Private harness, branch `feat/krusty-gate-honest-measurement`:

```
JAVA_HOME=~/.sdkman/candidates/java/25.0.2-zulu ./gradlew krustyReport \
  -Pkrusty.binary=<abs path to target/release/krusty> --max-workers=4 --console=plain
```

The binary must be the in-tree `target/release/krusty` — it resolves resources relative to its own
location, so a copy elsewhere makes every module fail with `exit=1 errors=0`.

Baselines on the same 28201-class corpus: master `644f8d30` = 11270 byte-identical (39.96%);
master plus the 22 open PRs = 17481 (61.99%).

Success is: the three blocked modules emit, the 13 shapes compile **and run correctly**, no gate
regressions, and the emitted suspend functions move toward kotlinc's spill sets.
