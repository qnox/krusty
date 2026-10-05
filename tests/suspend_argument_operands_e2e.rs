//! A call's operands stay on the operand stack across a suspending argument, as kotlinc's do.
//!
//! kotlinc lowers `join(first, other(), tail)` without temporaries: `first` is pushed, the
//! transformer's FixStack saves it into a local above the method's own around the suspension and
//! reloads it under the resumed result (`aload; swap`), and `tail` is pushed after it. Common
//! lowering held every operand of such a call in a temporary instead, so the state machine stored
//! the result and `tail` into two extra locals and reloaded all three.

use super::common;

const SOURCE: &str = r#"fun join(head: String, value: String, tail: String): String = head + value + tail

suspend fun head(): String = "O"

suspend fun other(): String = "K"

suspend fun stored(tail: String): String {
    val first = head()
    return join(first, other(), tail)
}
"#;

#[test]
fn operands_before_a_suspending_argument_stay_on_the_stack_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendArgumentOperands",
        SOURCE,
        &[
            "SuspendArgumentOperandsKt",
            "SuspendArgumentOperandsKt$stored$1",
        ],
    );
}

const CONSTRUCTED_SOURCE: &str = r#"class Pair(val first: Int, val second: Int)

suspend fun next(): Int = 2

suspend fun make(): Pair = Pair(1, next())
"#;

#[test]
fn a_constructor_keeps_its_operands_across_a_suspending_argument_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendConstructedOperands",
        CONSTRUCTED_SOURCE,
        &[
            "SuspendConstructedOperandsKt",
            "SuspendConstructedOperandsKt$make$1",
        ],
    );
}

/// A generic suspend callee hands its value-class result over as the box. Passed straight to a
/// value-class constructor, that box is unboxed once, at the resumed call.
const WRAPPED_SOURCE: &str = r#"@JvmInline
value class Inner(val x: Int)

@JvmInline
value class Outer(val inner: Inner)

object Source {
    @Suppress("UNCHECKED_CAST")
    suspend fun <T> resumed(value: Any?): T = value as T
}

suspend fun wrapped(value: Any?): Int = Outer(Source.resumed(value)).inner.x
"#;

#[test]
fn a_resumed_value_class_argument_is_unboxed_once_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendWrappedOperand",
        WRAPPED_SOURCE,
        &[
            "SuspendWrappedOperandKt",
            "SuspendWrappedOperandKt$wrapped$1",
        ],
    );
}
