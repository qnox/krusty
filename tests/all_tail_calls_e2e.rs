//! A suspend function whose every suspension point is a tail call forwards its continuation, as
//! kotlinc does.
//!
//! kotlinc builds no state machine when every suspension point is followed only by the return of
//! its value (`allSuspensionPointsAreTailCalls`): each call gets `$completion` and its `Object`
//! result is returned. That covers a `when` whose branches each return a call, and a call after an
//! early `return null`. krusty forwarded only a body with exactly one suspension point as its
//! final value, so these got a continuation class and a state machine.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const LIBRARY: &str = "package dependency\n\
    class Answer(val count: Int)\n\
    sealed interface Entry {\n\
    \x20   class Named(val id: String) : Entry\n\
    \x20   class Counted(val n: Int) : Entry\n\
    }\n\
    interface Engine {\n\
    \x20   suspend fun named(id: String): Answer\n\
    \x20   suspend fun counted(n: Int): Answer\n\
    }\n";

const SOURCE: &str = "import dependency.*\n\
    class Walker(private val engine: Engine) {\n\
    \x20   suspend fun resolve(entry: Entry): Answer =\n\
    \x20       when (entry) {\n\
    \x20           is Entry.Named -> engine.named(entry.id)\n\
    \x20           is Entry.Counted -> engine.counted(entry.n)\n\
    \x20       }\n\
    \x20   suspend fun maybe(id: String?): Answer? {\n\
    \x20       if (id == null) return null\n\
    \x20       return engine.named(id)\n\
    \x20   }\n\
    \x20   suspend fun either(flag: Boolean): Answer = when {\n\
    \x20       flag -> engine.counted(1)\n\
    \x20       else -> Answer(0)\n\
    \x20   }\n\
    }\n";

fn library() -> Option<std::path::PathBuf> {
    common::compile_libs("all-tail-calls", &[("Library", LIBRARY)])
}

#[test]
fn every_tail_call_forwards_its_continuation_like_kotlinc() {
    let Some(library) = library() else {
        eprintln!("skipping: dependency did not build");
        return;
    };
    let Some(built) = compare_with_kotlinc_plugin(
        "AllTailCalls",
        SOURCE,
        "Walker",
        &[common::stdlib_jar(), library],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    // No state machine in either compiler: nothing reads a continuation's `label`.
    for member in [
        "java.lang.Object resolve(",
        "java.lang.Object maybe(",
        "java.lang.Object either(",
    ] {
        for (compiler, text) in [("kotlinc", &built.reference), ("krusty", &built.krusty)] {
            let body = method_instructions(text, member);
            assert!(!body.is_empty(), "{compiler}: {member} not found");
            assert!(
                !body.iter().any(|insn| insn.contains(".label:I")),
                "{compiler} builds a state machine in {member}: {body:#?}"
            );
        }
    }
    // With no `when` subject or line bookkeeping of its own, the early-return shape is kotlinc's
    // instruction for instruction.
    let member = "java.lang.Object maybe(";
    assert_eq!(
        method_instructions(&built.krusty, member),
        method_instructions(&built.reference, member),
        "{member}"
    );
}

#[test]
fn forwarded_tail_calls_still_resume() {
    let main = format!(
        "{SOURCE}\
         import kotlin.coroutines.*\n\
         import kotlin.coroutines.intrinsics.*\n\
         var parked: Continuation<Answer>? = null\n\
         class Parking : Engine {{\n\
         \x20   override suspend fun named(id: String): Answer =\n\
         \x20       suspendCoroutineUninterceptedOrReturn {{ parked = it; COROUTINE_SUSPENDED }}\n\
         \x20   override suspend fun counted(n: Int): Answer = Answer(n)\n\
         }}\n\
         fun box(): String {{\n\
         \x20   var result = \"none\"\n\
         \x20   val walker = Walker(Parking())\n\
         \x20   val body: suspend () -> Unit = {{\n\
         \x20       val named = walker.resolve(Entry.Named(\"a\")).count\n\
         \x20       val counted = walker.resolve(Entry.Counted(4)).count\n\
         \x20       val absent = walker.maybe(null)\n\
         \x20       val present = walker.maybe(\"b\")?.count\n\
         \x20       result = \"$named:$counted:$absent:$present:${{walker.either(true).count}}:${{walker.either(false).count}}\"\n\
         \x20   }}\n\
         \x20   body.startCoroutine(Continuation(EmptyCoroutineContext) {{ it.getOrThrow() }})\n\
         \x20   parked!!.resume(Answer(5))\n\
         \x20   parked!!.resume(Answer(6))\n\
         \x20   return if (result == \"5:4:null:6:1:0\") \"OK\" else \"result $result\"\n\
         }}\n"
    );
    assert_eq!(
        common::expect_box_run_against_ref("all-tail-calls-run", LIBRARY, &main).as_deref(),
        Some("OK")
    );
}

/// A `Unit` function whose last statement discards another suspend function's result is no tail
/// call: its caller must be resumed with `Unit`, not with the callee's value. kotlinc gives it a
/// state machine; forwarding the continuation would hand the callee's `Int` to its caller.
#[test]
fn a_unit_function_discarding_a_result_keeps_its_state_machine() {
    let source = "suspend fun count(): Int = 1\n\
        suspend fun tick(): Unit {\n\
        \x20   count()\n\
        }\n";
    let built = compare_with_kotlinc_plugin(
        "UnitDiscardTail",
        source,
        "UnitDiscardTailKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let member = "java.lang.Object tick(";
    let body = method_instructions(&built.krusty, member);
    assert!(!body.is_empty(), "krusty: {member} not found");
    assert_eq!(
        body,
        method_instructions(&built.reference, member),
        "{member}"
    );
}

/// The discarded result never reaches the caller: once the callee resumes with its `Int`, the
/// `Unit` function completes its own caller with `Unit`.
#[test]
fn a_unit_function_discarding_a_result_resumes_its_caller_with_unit() {
    let main = "import kotlin.coroutines.*\n\
         import kotlin.coroutines.intrinsics.*\n\
         var parked: Continuation<Int>? = null\n\
         var observed: Any? = \"unset\"\n\
         class Gauge {\n\
         \x20   suspend fun count(): Int =\n\
         \x20       suspendCoroutineUninterceptedOrReturn { parked = it; COROUTINE_SUSPENDED }\n\
         \x20   suspend fun tick(): Unit {\n\
         \x20       count()\n\
         \x20   }\n\
         }\n\
         class Observer : Continuation<Unit> {\n\
         \x20   override val context: CoroutineContext get() = EmptyCoroutineContext\n\
         \x20   override fun resumeWith(result: Result<Unit>) { observed = result.getOrNull() }\n\
         }\n\
         fun box(): String {\n\
         \x20   Gauge::tick.startCoroutine(Gauge(), Observer())\n\
         \x20   if (observed != \"unset\") return \"completed before resuming: $observed\"\n\
         \x20   parked!!.resume(7)\n\
         \x20   return if (observed === Unit) \"OK\" else \"observed $observed\"\n\
         }\n";
    assert_eq!(
        common::compile_and_run_with_stdlib(main, "UnitDiscardTailRun").as_deref(),
        Some("OK")
    );
}
