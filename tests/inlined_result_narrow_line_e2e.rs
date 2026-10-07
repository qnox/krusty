//! A local that stores a classpath inline generic result narrows that result on the call's line.
//!
//! The repository-owned `neutral.fetch` returns the erased `Object` produced by a generic member.
//! kotlinc's `visitVariable` marks the declaration before the `checkcast` back to the local's type,
//! and the inlined body has forgotten the caller's line, so that cast — not the store after it —
//! starts the declaration's line. A custom declaration keeps this regression independent of any
//! stdlib intrinsic or member-name special case.

use super::common;

const LIBRARY: &str = r#"package neutral

class Slot<T>(private val value: T) {
    fun read(): T = value
}

inline fun <T> fetch(slot: Slot<T>): T = slot.read()
"#;

const SOURCE: &str = r#"import neutral.Slot
import neutral.fetch

fun lookup(slot: Slot<String>): String {
    val value = fetch(slot)
    return value
}

fun withIt(slot: Slot<String>, action: (String) -> String): String {
    val value = fetch(slot)
    return action(value)
}
"#;

#[test]
fn an_inlined_generic_result_is_narrowed_on_the_calls_line_like_kotlinc() {
    let library = common::kotlinc_lib_out(&[("NeutralInline.kt", LIBRARY)])
        .expect("reference kotlinc builds the neutral inline dependency");
    common::assert_classes_identical_to_kotlinc_against(
        "InlinedResultNarrow",
        SOURCE,
        &["InlinedResultNarrowKt"],
        &[library],
    );
}
