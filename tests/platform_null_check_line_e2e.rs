//! An implicit null check of a platform value produced by an inlined call owns a line entry.
//!
//! The inlined `apply` restores the caller's line after its body and leaves that line forgotten.
//! kotlinc marks the checked expression's line again at the `checkNotNullExpressionValue`
//! sequence, so the check gets its own entry even though the same line is already in effect.

use super::common;

const SOURCE: &str = r#"
import java.util.Collections

fun single(): List<String> = Collections.synchronizedList(ArrayList<String>()).apply { this.add("a") }

fun multi(): List<String> = Collections.synchronizedList(ArrayList<String>()).apply {
    this.add("a")
    this.add(0, "b")
}

fun local(): Int {
    val list: MutableList<String> = Collections.synchronizedList(ArrayList<String>()).also { it.add("c") }
    return list.size
}

fun box(): String {
    if (single() != listOf("a")) return "single"
    if (multi() != listOf("b", "a")) return "multi"
    return if (local() == 1) "OK" else "local"
}
"#;

#[test]
fn an_implicit_null_check_after_an_inlined_call_marks_its_line() {
    common::assert_classes_identical_to_kotlinc(
        "PlatformNullCheckLine",
        SOURCE,
        &["PlatformNullCheckLineKt"],
    );
    common::expect_box_same_as_kotlinc(SOURCE, "PlatformNullCheckLine");
}
