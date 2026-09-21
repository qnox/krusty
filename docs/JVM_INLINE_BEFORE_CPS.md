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
`$suspended`) are placed at `max_locals..`, above every body and inline local, which is how kotlinc
numbers them. The entry prologue, the `tableswitch` dispatch, the per-suspension spill block, the
`COROUTINE_SUSPENDED` check and the resume blocks are all emitted through the ordinary `CodeBuilder`
label and frame API, so frames are produced the way every other method's frames are produced. The
body emission itself is identical to pass 1, which is what makes pass 1's slot numbering and spill
sets valid for pass 2.

The emitted order inside a suspension is kotlinc's, not a convenient one: the call's operands are
pushed first (including the continuation), *then* the spill block runs stack-neutrally underneath
them, then `label` is stored, then the call. §3 and §7 both show that order in the reference output.

Nothing synthetic may survive into the method. The resume rejoin point cannot be recorded as a byte
offset before the splice — relocation widens `ldc` to `ldc_w`, so sizes move — so it is recovered by
stepping the known instruction index from the splice's `byte_start` through the *relocated*
instructions. A marker instruction erased to `nop`s would be simpler and is available
(`classfile::CoroutineMarker`), but `nop`s the reference compiler does not emit are a byte
difference, so they are used only where the bytes are thrown away.

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
| 3 | Pass 2 — the state machine, for the **currently bailing** shapes only, in kotlinc's instruction order. The IR machine still owns every method it owns today, so the 11270 byte-identical classes cannot move. Fixture A (§7) lands here, asserting `box()` against the reference compiler and a class-file diff against it. | green + fixture A runs + byte-identical |
| 4 | Continuation-class finalization: spill fields and `@DebugMetadata` become products of the bytecode pass for the new-machine methods. | green |
| 5 | Under-stack spilling (§5.3 v2). Fixture B lands here. | green + fixture B runs |
| 6 | Measure the corpus. Expect the three blocked modules to emit (≈987 classes). Report before/after. | corpus report |
| 7 | Migrate the remaining suspend methods onto the emit-time machine, one shape family at a time, each with a byte-parity delta. | per-step byte delta |
| 8 | Retire the IR machine for JVM; consider the `inline_body_plan` deletion. | measured |

Steps 3 and 5 can only be *correct* until §7a lands; they become byte-identical once it does. §7a is
independent of everything else here and can go first.

Every fixture asserts the behaviour it is supposed to have, never the bail it currently gets:
`AGENTS.md` rule 9 rejects a diagnostics test that asserts only rejection. So a shape's fixture
lands in the step that makes it work, not before, and the "before" side of the comparison lives in
this note (§7) rather than in a characterization test.

Step 3 is the one that pays for the work; steps 1 and 2 exist so that step 3 is small.

## 6a. Landed state (2026-09-21)

Steps 3 and 5 are in, in a form the note did not anticipate: the machine reads its spill set off
the **post-splice bytecode of a first emission** and is then built by a second one, rather than a
separate bytecode CPS pass. `src/jvm/ir_emit/coroutine_machine.rs` holds the plan, the continuation
class and the discovery; the control graph and liveness of step 1 feed it.

Working today, each with a fixture in `tests/suspend_in_spliced_inline_e2e.rs`:

| Shape | Fixture |
| --- | --- |
| One suspension in a spliced lambda | `a_suspension_inside_a_spliced_inline_lambda_runs` |
| Two suspensions in one spliced body | `two_suspensions_in_one_spliced_body_run` |
| A reference local live across one | `a_reference_local_survives_a_spliced_suspension` |
| A conditional suspension | `a_conditional_suspension_in_a_spliced_body_runs` |
| A `Long` (two-slot) local live across one | `a_long_local_survives_a_spliced_suspension` |
| Through stdlib `run` / `let` / `repeat` | `…_spliced_stdlib_{run,let,repeat}_lambda_runs` |
| An instance method's machine | `a_suspension_inside_a_spliced_lambda_of_a_member_runs` |
| A MIXED function (its own suspension plus a spliced one) | `a_function_that_suspends_both_in_and_outside_a_spliced_body_runs` |
| A mixed function inside `try`/`finally` | `a_mixed_function_inside_try_finally_runs` |
| A PRIVATE member's machine (via `access$<name>`) | `…_of_a_private_member_runs` |
| A body spliced into a `try` region (`runCatching { susp() }`) | `a_suspension_inside_a_spliced_try_region_runs` |
| A value-`try` in the spliced body around the suspension (`try { susp() } catch …`) | `a_value_try_whose_arm_is_the_suspension_runs`, `…_computes_with_the_suspension_runs`, `…_binds_the_suspension_first_runs` |
| That `try`'s `catch` running, on the direct path and on a failed resumption | `a_value_try_catches_a_throw_on_the_direct_path`, `a_value_try_catches_a_failed_resumption` |

**A value-`try` in a spliced body** gets the value-`try` desugar a function body gets
(`desugar_spliced_value_try` in `src/jvm/suspend/hoisting.rs`), before the body is hoisted. A
suspending arm's value is the call's raw `Object`; stored straight into the `try`'s result slot it
would be described as a scalar the frame never had. The desugar binds each arm to a typed local
first. The body's own tail value is typed by the `try` itself (or the coercion around it), never by
the enclosing function's return type — that type is right only for a non-local `return` inside the
body, which is the one statement it is used for. A value-`try` nested deeper in an expression
(`1 + try { … }`) is not reached and still declines (`suspends_in_a_value_try`, checked AFTER
normalization). A spliced body with a non-local `return` declines too (`spliced_body_returns`):
under the machine that return has to yield the CPS `Object`, and the emitter does not box it yet.

**The resume point is inside the body.** A failed resumption (`Result.Failure` in the
continuation's `result`) is rethrown at a `Resume` marker emitted right after the suspension's
`areturn` — inside any `try` the body wraps around the call — and falls into the join. The
dispatch's restore blocks, which sit after the body and outside every range it declares, only
restore spills and jump there. Rethrowing in the restore block let the callee's exception escape a
`catch` written around the suspension.

**A spliced lambda's own `try` ranges** are relocated into the caller's exception table with the
body (`LambdaSplice::handlers`); before that only the dependency's handlers were, and a `catch`
written in the lambda was dead code — silently, with or without a suspension. When the host holds
values on the operand stack under the lambda's value (`acc + f(x)`: `sumOf`, `fold`), a handler
would be entered without them, so such a body declines the splice (the lambda stays a closure; a
`MustInline` callee then bails). kotlinc spills that prefix into locals around the body; this
splice does not yet.

Still bailing, by design: an OPEN member (the continuation re-enters with `invokevirtual`, which
must reach this very body — kotlinc uses a `$suspendImpl` static for these), and any shape where the
emitter declines the splice — there the lambda is a real closure, its standalone `invoke` has been
taken away, and the compile is declined rather than emitting an `invokedynamic` to a method that
does not exist.

**A MIXED function** — one with a suspension of its own AND one inside a spliced lambda — is taken
whole by the emit-time machine: one method has one dispatch, so the two kinds cannot be split
between two machines. No function that compiles today changes hands, because a function with a
spliced suspension does not compile today.

**The machine's shape is now kotlinc's.** The dispatch's `tableswitch` targets one restore block per
state; each restores that state's spills, pushes the resumed value and jumps to the join inside the
body. Restoring before re-entry is what makes a suspension inside a `try` verify: a handler's frame
claims the locals the protected code assigned, and an edge from the region's own start cannot
produce them. The blocks are emitted AFTER the body, so they cannot move the slots the body
allocates — the plan was read from a first emission that had none of this code in it.

**A lambda's `inline_body` is not evidence** that its body runs in this frame: lowering attaches one
wherever it can, including to the lambda of an ordinary function. Only the operand of a call to an
inline function is spliced, and both the suspension walk and the impl-suppression walk apply that
rule.

**Correctness before byte parity.** Two deliberate divergences from kotlinc remain: the spill set is
widened to every local the merged frames CLAIM at a resume (kotlinc spills exactly its liveness
set), and suspensions inside a spliced body are hoisted to statement temps before the machine reads
them. Both are what makes the shapes above run; neither is byte-identical. §7a still gates parity.

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

## 7a. Prerequisite: the splice is not byte-identical without any suspension

Byte-identity for these shapes is gated on a defect that has nothing to do with coroutines. Take
fixture A's library and make `one` an ordinary function, so no suspension is involved at all:

```kotlin
fun one(v: Int): Int = v + 1
fun many(v: Int): Int = twice(v) { one(it) }
```

krusty compiles this today, and the class file differs from the reference compiler's. Two
independent causes, both in the existing splice path, both reproducible with reference types as
well as primitives:

**(a) An extra copy of the inline parameter.** The reference compiler stores the argument once:

```
   0: iload_0 ; istore_1        // x, the inline parameter
   2: iconst_0; istore_2        // $i$f$twice
```

krusty stores it twice, into a caller slot and then into the host's parameter slot:

```
   0: iload_0 ; istore_1
   2: iload_1 ; istore_2        // a second copy
   4: iconst_0; istore 4        // the marker, now two slots higher
```

Every later local is shifted, so every subsequent instruction that names a slot differs.

**(b) The lambda's `invoke` adapter survives the splice.** `FunctionN.invoke` is erased to
`(Object)Object`, so an argument is boxed and cast on the way in and unboxed on the way out. Once
the lambda is spliced there is no `invoke` left and the reference compiler passes the value
directly; krusty keeps the adapter:

```
  reference:  iload 5 ; invokestatic one:(I)I ; istore 5
  krusty:     iload_2 ; iload 6 ; iadd
              invokestatic Integer.valueOf ; checkcast Integer ; invokevirtual intValue
              istore 8 ; iload 8 ; invokestatic one:(I)I
              invokestatic Integer.valueOf ; checkcast Number ; invokevirtual intValue ; istore 7
```

With a `String` channel the boxing disappears but the `checkcast` does not, and (a) remains — so
these are two separate defects, not one.

Traced with `KRUSTY_TRACE=splice`, (a) is itself two causes:

```
[splice] inline operands [(16, GetValue(1), Int), (14, Lambda { impl_fn: 2, … })]
[splice] inline prologue descriptor=(ILkotlin/jvm/functions/Function1;)I
         stores=[(2, istore), (3, astore)] lambda_slots={3} prologue=[istore_2]
```

* **a1 — the argument is read from a local that should not exist.** The operand is `GetValue(1)`,
  not the parameter (`many`'s parameter is value index 0, slot 0). Something below the emitter binds
  the inline call's argument to its own local, which is the `iload_0; istore_1` pair, and which
  raises `base` from 1 to 2. The reference compiler stores the argument straight into the host's
  parameter slot.
* **a2 — the spliced-away lambda's parameter slot stays reserved.** `lambda_slots={3}` is correctly
  skipped by the prologue, but the host body's locals are shifted by a flat `base`, so slot 3 is
  reserved for a `Function1` that no longer exists and every host local above it sits one slot too
  high. The reference compiler compacts it away: its `$i$f$twice` is at slot 2, krusty's at 4.

A third shows up in a call with **no** value argument, `fun a(v: Int) = once { v }`:

```
  reference:  iconst_0; istore_1     // $i$f$once      (host depth marker)
              iconst_0; istore_2     // $i$a$-once-…   (LAMBDA depth marker)
              iload_0 ; istore_3     // r
  krusty:     iconst_0; istore_2     // one marker only, and two slots up
              …boxed lambda result…  ; istore_3
```

* **a3 — the spliced lambda's own inline-depth marker is missing.** The reference compiler opens an
  inlined lambda body with `iconst_0; istore` into a `$i$a$-<callee>-<caller>` local, exactly as it
  opens an inlined function body with `$i$f$<callee>`. krusty emits the host's marker (it comes
  along inside the relocated host body) but never the lambda's, because the lambda body is emitted
  from IR, not relocated.

Note that `a` shows a1 does *not* fire without a value argument: the extra local appears only when a
value operand accompanies the inline-body lambda.

So §7a is four fixes — a1, a2, a3 and (b) — each affecting every spliced inline call with a lambda,
none of them coroutine-related:

The debug tables are a third dimension. For the same non-suspending `many`, the reference compiler
emits a complete `LocalVariableTable` over the inlined frame:

```
   Start  Length  Slot  Name                       Signature
      24       5     6  $i$a$-twice-MainKt$many$1  I
      21       8     5  it                         I
      31       8     5  r$iv                       I
       4      39     2  $i$f$twice                 I
       6      37     3  acc$iv                     I
       9      34     4  i$iv                       I
       2      41     1  x$iv                       I
       0      44     0  v                          I
```

krusty's table has one entry, `v`. The reference class also carries a `SourceDebugExtension` (the
SMAP that maps inlined instructions back to the library's source); krusty emits none. The two class
files are 1175 and 947 bytes.

So §7a is six gaps, all reachable without a coroutine:

| | defect | effect | status |
| --- | --- | --- | --- |
| a1 | a value operand beside an inline-body lambda is bound to its own local | one extra local + `iload/istore` pair; shifts every later slot | **fixed** |
| a2 | the spliced-away lambda parameter's slot stays reserved | shifts every host local one slot up | **fixed** |
| a3 | the spliced lambda body has no `$i$a$` inline-depth marker | a missing `iconst_0; istore` and a slot | **fixed** |
| a4 | no `LocalVariableTable` entries for the inlined frame | the whole table, including the `$iv` names read from the dependency's own table | **fixed** |
| a5 | no `SourceDebugExtension` | the entire SMAP attribute | open |
| a6 | no `LineNumberTable` entries for the inlined body | the reference compiler records the *library's* lines inside the caller, which is what a5 then resolves | open |
| b | the lambda's erased `invoke` adapter survives the splice | `valueOf` / `checkcast` / `intValue` the reference compiler does not emit | **fixed** (adjacent pairs) |

**Result of the four code-level fixes.** For the shape this note is built around — a classpath
`inline fun twice(x, f)` with a loop, called with a lambda — the emitted method is now the reference
compiler's exact instruction sequence, over the same slots:

```
   0: iload_0 ; istore_1        // x
   2: iconst_0; istore_2        // $i$f$twice
   4: iconst_0; istore_3        // acc
   6: iconst_0; istore 4        // i
   9: iload 4 ; iconst_2 ; if_icmpge 42
  15: iload_1 ; iload 4 ; iadd ; istore 5     // it
  21: iconst_0; istore 6        // $i$a$
  24: iload 5 ; invokestatic one:(I)I ; istore 5
  31: iload_3 ; iload 5 ; iadd ; istore_3
  36: iinc 4, 1 ; goto 9
  42: iload_3 ; ireturn
```

`classpath_inline_splice_parity_e2e` pins it, comparing one method's disassembly against the
reference compiler's with pool indices normalized away.

The `LocalVariableTable` matches too, entry for entry — same starts, lengths, slots and names:

```
   Start  Length  Slot  Name
      24       5     6  $i$a$-twice-MainKt$many$1
      21       8     5  it
      31       8     5  r$iv
       4      39     2  $i$f$twice
       6      37     3  acc$iv
       9      34     4  i$iv
       2      41     1  x$iv
       0      44     0  v
```

The dependency's entries are relocated through the same `old2new` and slot map the instructions
travel through, so they cannot drift from the code they describe. The lambda's own two have no other
source and are named here: the parameter from the lambda's recorded parameter names, and the marker
from the reference compiler's own spelling, `$i$a$-<callee>-<owner>$<enclosing>$<ordinal>`. The
order is the reference compiler's: the innermost inlining first, and within a lambda its marker
before its parameters, which is the reverse of the order they are stored in.

What still separates the two class files is a5, a6, and the constant pool's interning order — 1068
bytes against 1175.

a1, a2 and a3 all move slot numbers, so they are fixed and measured together, with the corpus as the
judge. a4 and a5 are additive attributes and independent of the rest. **b** is the only one that
needs an analysis rather than a rule: cancelling a box against the unbox that immediately consumes
it, and the reverse, at a substituted invoke site.

This is the honest size of "byte-identical" for spliced inline code. It is a parity programme of its
own, and the coroutine ordering change depends on all of it — a machine emitted in exactly the
reference compiler's instruction order still cannot produce an identical class file while the body
it wraps differs by six causes.

Consequence for the ordering work: a coroutine machine emitted in the reference compiler's exact
instruction order still cannot produce an identical class file while the body it wraps differs. Both
defects must be fixed first, as their own change — they are splice-representation bugs, they affect
every spliced inline call with a lambda whether or not it suspends, and fixing them moves byte
parity on code that compiles today. The coroutine machine then lands on a body that already matches.

## 7b. The reference compiler's inline slot allocation is not a remap

a2 and a3 both need to know which slot an inlined local gets, and the answer is not a function of
the host's numbering. Four probes, each `inline fun … { val r = f(…); return r + 1 }` with a
different lambda arity, plus one with a host local declared before the call
(`pre(a) { … }`, body `val b = a + 1; val r = f()`):

| probe | host locals | reference result |
| --- | --- | --- |
| `p0 { v }` | `f`, `$i$f`, `r` | `$i$f`=1, `$i$a$`=2, **`r`=3** |
| `p1 { it + v }` | same | `$i$f`=1, `it`=2, `$i$a$`=3, **`r`=4** |
| `p2 { x, y -> … }` | same | `$i$f`=1, `y`=2, `x`=3, `$i$a$`=4, **`r`=5** |
| `pre(v) { v }` | `a`, `f`, `$i$f`, `b`, `r` | `a`=1, `$i$f`=2, `b`=3, `$i$a$`=4, **`r`=4** |

The first three are consistent with "assign in first-store order" — the host local declared after the
call lands above the lambda's locals. The fourth is not: there `r` **reuses** the marker's slot, and
the two share slot 4 with disjoint ranges ([11,12) and [14,18)), exactly as `it` and `r$iv` share
slot 5 in the `twice` case of §7a. Yet `p0` has the same structure as `pre` — one lambda local, a
host local declared after the call — and does *not* reuse.

So it is neither a flat shift, nor a compaction, nor a plain live-range reuse. The remaining
candidate is the reference implementation's own remapper, whose allocation order is a property of
that code rather than of the bytecode it produces. Deriving it from more samples is guesswork;
it should be read from the implementation.

What is implemented today is "the first slot free at the invoke, from the dependency's own
`LocalVariableTable`". That reproduces the reference result for §7a's motivating case and for
`pre`, and is off by the lambda's local count for the `p0`/`p1`/`p2` family, where the inline
function's only local is declared after the call. It is recorded here as an approximation, not as
the rule; `classpath_inline_splice_parity_e2e` deliberately does not pin that family's instructions,
because asserting a shape known not to match yet is not a test.

The approximation is safe, not merely close: the lambda's locals occupy slots no host local holds at
the invoke, and any host local declared later is declared after the inlined body has ended, so a
shared slot is always dead at the point it is reused.

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
regressions, and the emitted suspend functions are **byte-identical to the reference compiler** —
not merely correct. That is the bar for this work, so the machine is built to kotlinc's instruction
order from the first landing rather than refined toward it: these classes emit nothing today, so
there is no byte-parity baseline to protect and no reason to accept a shape that would have to be
redone.

Byte-identity pins down more than the instruction order. The spill set must match exactly, which
means the liveness analysis has to agree with the reference compiler's own examiner — including its
rematerialization of constant locals (the `$i$f$…` inline-depth markers are re-established with
`iconst_0; istore` on resume rather than spilled, §3 and §7), the reverse restore order, and which
spilled references get `nullOutSpilledVariable`. That agreement is the part most likely to need
iteration against the corpus; it is also the part the analysis in `src/jvm/suspend/cps/` exists to
make measurable rather than guessed.
