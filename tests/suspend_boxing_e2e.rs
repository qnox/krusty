//! kotlinc runs its coroutine transformer over every named suspend function, one that never
//! suspends included: the transformer boxes a primitive through
//! `kotlin.coroutines.jvm.internal.Boxing` rather than its wrapper's `valueOf`, and a body with no
//! suspension point then keeps its code with no state machine.

use super::common;

/// A top-level function and a member returning primitives, neither of which suspends.
#[test]
fn a_suspend_function_that_never_suspends_boxes_through_boxing() {
    const SRC: &str = "suspend fun next(x: Int): Int = x + 1\n\
\n\
suspend fun widen(x: Int, y: Long): Any = if (x > 0) y else x\n\
\n\
class Counter(private val start: Int) {\n\
    suspend fun current(): Int = start\n\
}\n";
    common::byte_diff_against_kotlinc_cp(
        "NeverSuspends",
        SRC,
        "NeverSuspendsKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("NeverSuspendsKt byte-identical to kotlinc");
    common::byte_diff_against_kotlinc_cp("NeverSuspends", SRC, "Counter", &[common::stdlib_jar()])
        .expect("reference kotlinc is provisioned")
        .expect("Counter byte-identical to kotlinc");
}

/// A transformed body interns `Boxing` when it is installed: the constant pool is laid out again
/// around it, in the order kotlinc's writer interns the transformed method.
#[test]
fn a_transformed_body_lays_its_constants_out_like_kotlinc() {
    const SRC: &str = "suspend fun step(x: Int): Int = x\n\
\n\
suspend fun twice(x: Int): Int {\n\
    step(x)\n\
    return x + 1\n\
}\n";
    common::byte_diff_against_kotlinc_cp(
        "TransformedPool",
        SRC,
        "TransformedPoolKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("TransformedPoolKt byte-identical to kotlinc");
}

/// A suspend conversion's adapter `invoke` is a suspend function of the reference class, and the
/// transformer takes it too.
#[test]
fn a_suspend_conversion_adapter_boxes_through_boxing() {
    const SRC: &str = "fun one(): Int = 1\n\
\n\
fun take(f: suspend () -> Int) {}\n\
\n\
fun adapt() = take(::one)\n";
    common::byte_diff_against_kotlinc_cp(
        "SuspendAdapter",
        SRC,
        "SuspendAdapterKt$adapt$1",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .expect("SuspendAdapterKt$adapt$1 byte-identical to kotlinc");
}
