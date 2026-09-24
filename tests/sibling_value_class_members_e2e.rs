//! Calling a member of a value class declared in ANOTHER source file of the same module.
//!
//! The declaring file realizes every member of a value class as a static implementation over the
//! carrier: `getDoubled-impl(I)I`, or the value-class hash when the member's signature mentions a
//! value class (`plus-<hash>(II)I`). A call from a sibling file used to keep the instance shape —
//! `box-impl` followed by `invokevirtual Distance.getDoubled()I` — which names a method no
//! value class has and fails at run time:
//!
//! ```text
//! NoSuchMethodError: 'int units.Distance.getDoubled()'
//! ```
//!
//! kotlinc calls the static implementation with the carrier as argument zero; so does krusty now,
//! deriving the name from the member's declared signature exactly as the declaring file does.
use super::common;

const DISTANCE: &str = "package units\n\
    \n\
    interface Measured {\n\
    \x20   fun area(): Int\n\
    }\n\
    \n\
    @JvmInline\n\
    value class Distance(val meters: Int) : Measured {\n\
    \x20   val doubled: Int get() = meters * 2\n\
    \x20   val self: Distance get() = this\n\
    \x20   fun next(): Int = meters + 1\n\
    \x20   fun plus(other: Distance): Distance = Distance(meters + other.meters)\n\
    \x20   fun label(scale: Int, prefix: String): String = prefix + (meters * scale)\n\
    \x20   fun shifted(by: Int = 5): Int = meters + by\n\
    \x20   override fun area(): Int = meters * meters\n\
    \x20   override fun toString(): String = \"Distance(\" + meters + \")\"\n\
    }\n";

const DISTANCE_CALLS: &str = "import units.Distance\n\
    \n\
    fun doubledOf(d: Distance): Int = d.doubled\n\
    fun selfOf(d: Distance): Distance = d.self\n\
    fun nextOf(d: Distance): Int = d.next()\n\
    fun sumOf(a: Distance, b: Distance): Distance = a.plus(b)\n\
    fun labelOf(d: Distance): String = d.label(2, \"x\")\n\
    fun shiftedOf(d: Distance): Int = d.shifted()\n\
    fun areaOf(d: Distance): Int = d.area()\n\
    fun textOf(d: Distance): String = d.toString()\n";

const DISTANCE_BOX: &str = "import units.Distance\n\
    \n\
    fun box(): String {\n\
    \x20   val d = Distance(3)\n\
    \x20   val seen = listOf(\n\
    \x20       doubledOf(d), selfOf(d).meters, nextOf(d), sumOf(d, Distance(4)).meters,\n\
    \x20       shiftedOf(d), areaOf(d),\n\
    \x20   ).joinToString(\",\") + \";\" + labelOf(d) + \";\" + textOf(d)\n\
    \x20   return if (seen == \"6,3,4,7,8,9;x6;Distance(3)\") \"OK\" else seen\n\
    }\n";

#[test]
fn a_sibling_value_class_member_is_called_through_its_static_implementation() {
    let sources = [
        ("Distance.kt", DISTANCE),
        ("Calls.kt", DISTANCE_CALLS),
        ("Box.kt", DISTANCE_BOX),
    ];
    // Getters, a getter and a function whose signatures mention the value class (hashed names),
    // several parameters, an omitted default (`-impl$default`), an interface override and a
    // `toString` override: every call site matches kotlinc byte for byte.
    let pair = common::ModuleClassPair::compile(&sources, "CallsKt");
    if pair.krusty != pair.kotlinc {
        let (kotlinc, krusty) = pair.method_code("CallsKt", "doubledOf-amH63Mg");
        panic!(
            "CallsKt differs from kotlinc's ({} B vs {} B)\n--- kotlinc doubledOf ---\n{kotlinc}\
             --- krusty doubledOf ---\n{krusty}",
            pair.krusty.len(),
            pair.kotlinc.len()
        );
    }
    assert_eq!(common::kotlinc_box_files_result(&sources, "BoxKt"), "OK");
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK"),
        "the declaring file's static implementations are the ones the sibling file calls"
    );
}

const OUTCOME: &str = "package outcomes\n\
    \n\
    @JvmInline\n\
    value class Outcome<out V, out E> internal constructor(private val raw: Any?) {\n\
    \x20   val isSuccess: Boolean get() = raw !is Failed<*>\n\
    \x20   @Suppress(\"UNCHECKED_CAST\")\n\
    \x20   val value: V get() = raw as V\n\
    \x20   fun valueOr(fallback: @UnsafeVariance V): V = if (isSuccess) value else fallback\n\
    }\n\
    \n\
    internal class Failed<out E>(val error: E)\n";

const OUTCOME_CALLS: &str = "package app\n\
    \n\
    import outcomes.Outcome\n\
    \n\
    fun succeeded(o: Outcome<Int, String>): Boolean = o.isSuccess\n\
    fun valueOf(o: Outcome<Int, String>): Int = o.value\n\
    fun valueOr(o: Outcome<Int, String>): Int = o.valueOr(-1)\n\
    \n\
    fun box(): String {\n\
    \x20   val o: Outcome<Int, String> = Outcome(42)\n\
    \x20   val seen = \"\" + succeeded(o) + \",\" + valueOf(o) + \",\" + valueOr(o)\n\
    \x20   return if (seen == \"true,42,42\") \"OK\" else seen\n\
    }\n";

/// A generic value class in another package whose carrier is `Any?`: the static implementation
/// takes `Object`, and the local holding the value is already that carrier, so kotlinc passes it
/// directly — no `box-impl`/`unbox-impl` round trip, which a generic `Object` slot would suggest.
#[test]
fn a_generic_sibling_value_class_member_takes_its_nullable_any_carrier() {
    let sources = [("Outcome.kt", OUTCOME), ("Calls.kt", OUTCOME_CALLS)];
    let pair = common::ModuleClassPair::compile(&sources, "app/CallsKt");
    for method in [
        "succeeded-OHYIKC4",
        "valueOf-OHYIKC4",
        "valueOr-OHYIKC4",
        "box",
    ] {
        let (kotlinc, krusty) = pair.method_code("app/CallsKt", method);
        assert_eq!(krusty, kotlinc, "app/CallsKt.{method} instructions");
    }
    assert_eq!(
        common::kotlinc_box_files_result(&sources, "app.CallsKt"),
        "OK"
    );
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK")
    );
}

const TICKET: &str = "package queue\n\
    \n\
    @JvmInline\n\
    value class Ticket(val number: Int) {\n\
    \x20   suspend fun after(other: Ticket): Int = number - other.number\n\
    \x20   suspend fun position(): Int = number\n\
    }\n";

const TICKET_CALLS: &str = "import queue.Ticket\n\
    import kotlin.coroutines.Continuation\n\
    import kotlin.coroutines.EmptyCoroutineContext\n\
    import kotlin.coroutines.startCoroutine\n\
    \n\
    fun box(): String {\n\
    \x20   var seen = \"\"\n\
    \x20   suspend { \"\" + Ticket(9).after(Ticket(4)) + \",\" + Ticket(2).position() }\n\
    \x20       .startCoroutine(Continuation(EmptyCoroutineContext) { seen = it.getOrThrow() })\n\
    \x20   return if (seen == \"5,2\") \"OK\" else seen\n\
    }\n";

/// A suspend member's hash covers its continuation parameter too (`after-iCk_5KM`, where the same
/// signature without `suspend` hashes differently). The sibling call site derives the name with
/// the call's checked suspend fact; the declaration carries kotlinc's name, so a successful run
/// shows the call site spells it the same way.
#[test]
fn a_sibling_suspend_value_class_member_keeps_the_continuation_in_its_hash() {
    let sources = [("Ticket.kt", TICKET), ("Calls.kt", TICKET_CALLS)];
    // Both compilers declare the implementations under these names (`method_code` fails on a
    // missing method). Their bodies are not compared: krusty boxes a suspend result through
    // `Integer.valueOf` where kotlinc uses `Boxing.boxInt`, which is unrelated to member naming.
    let pair = common::ModuleClassPair::compile(&sources, "queue/Ticket");
    for method in ["after-iCk_5KM", "position-impl"] {
        pair.method_code("queue/Ticket", method);
    }
    assert_eq!(common::kotlinc_box_files_result(&sources, "CallsKt"), "OK");
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&sources).as_deref(),
        Some("OK")
    );
}
