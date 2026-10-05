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
