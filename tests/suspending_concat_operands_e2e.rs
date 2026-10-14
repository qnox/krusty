//! A string concatenation keeps its parts direct across a suspending part, as kotlinc does.
//!
//! kotlinc lowers `head + tail()` by creating the `StringBuilder`, appending `head`, and then
//! calling `tail()`. The coroutine transformer saves the builder across the suspension. Common
//! lowering used to copy every part into a temporary once one part suspended, so the builder was
//! created only after the resumed call.

use super::common;

const SOURCE: &str = r#"suspend fun tail(): String = "K"

suspend fun join(head: String): String = head + tail()

suspend fun template(head: String, count: Int): String = "$head-${tail()}-$count"

suspend fun smart(value: Any): String {
    if (value !is String) return "FAIL"
    return value + tail()
}
"#;

#[test]
fn a_concatenation_keeps_its_parts_direct_across_a_suspension_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SuspendingConcat",
        SOURCE,
        &[
            "SuspendingConcatKt",
            "SuspendingConcatKt$join$1",
            "SuspendingConcatKt$smart$1",
            "SuspendingConcatKt$template$1",
        ],
    );
}
