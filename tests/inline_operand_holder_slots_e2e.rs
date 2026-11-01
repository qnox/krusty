//! The holder an inline call stages its operand in is not an argument-order temporary. kotlinc
//! stores such an operand once it is evaluated, so an inline call the operand makes keeps its frame
//! marker's slot and the operand takes the next one. Only a call whose named arguments reorder it
//! declares its temporaries before their values, as kotlinc's variables are.

use super::common;

const INLINE_RECEIVER: &str = r#"inline fun two(): String = "2"
fun box(): String = two().let { if (it == "2") "OK" else it }
"#;

#[test]
fn an_inline_calls_operand_takes_the_slot_after_its_inline_receivers_marker() {
    let classes = common::classes_against_kotlinc_module(&[("InlineReceiver.kt", INLINE_RECEIVER)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn an_inline_calls_operand_staged_after_an_inline_receiver_runs() {
    common::expect_box_same_as_kotlinc(INLINE_RECEIVER, "InlineReceiver");
}

const REORDERED_INLINE_ARGUMENT: &str = r#"inline fun two(): String = "2"
fun pair(first: String, second: String): String = first + second
fun box(): String {
    val joined = pair(second = two(), first = "1".repeat(1))
    return if (joined == "12") "OK" else joined
}
"#;

#[test]
fn a_reordered_arguments_temporary_precedes_its_inline_values_marker() {
    let classes = common::classes_against_kotlinc_module(&[(
        "ReorderedInlineArgument.kt",
        REORDERED_INLINE_ARGUMENT,
    )]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_reordered_argument_with_an_inline_value_runs_in_source_order() {
    common::expect_box_same_as_kotlinc(REORDERED_INLINE_ARGUMENT, "ReorderedInlineArgument");
}
