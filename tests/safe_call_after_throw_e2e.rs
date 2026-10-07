//! A safe call whose selector throws keeps the receiver temporary when constant-condition
//! elimination removes its unreachable continuation and retains that continuation's label.
//!
//! kotlinc stores the receiver and folds `aload v; ifnull L; aload v` to `dup` only when nothing
//! falls into `L`. After a loop or an `if`, elimination deletes the unreachable continuation of
//! `throw` and leaves its label in front of `L`, so the temporary stays.

use super::common;

const SRC: &str = "\
fun afterIf(flag: Boolean) {\n\
    var lastException: Exception? = null\n\
    if (flag) lastException = Exception()\n\
    lastException?.let { throw it }\n\
}\n\
fun afterLoop(n: Int) {\n\
    var lastException: Exception? = null\n\
    var i = 0\n\
    while (i < n) {\n\
        i++\n\
        lastException = Exception()\n\
    }\n\
    lastException?.let { throw it }\n\
}\n\
fun afterFor(items: List<String>) {\n\
    var lastException: Exception? = null\n\
    for (item in items) {\n\
        try {\n\
            if (item.length < 0) throw Exception()\n\
        } catch (e: Exception) {\n\
            lastException = e\n\
        }\n\
    }\n\
    lastException?.let { throw it }\n\
}\n\
";

#[test]
fn throwing_safe_calls_after_control_flow_are_byte_identical_to_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "SafeCallAfterThrow",
        SRC,
        &["SafeCallAfterThrowKt"],
    );
}

#[test]
fn a_throwing_safe_call_after_a_loop_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SRC}\
             fun box(): String {{\n\
             \x20   afterIf(false)\n\
             \x20   afterLoop(0)\n\
             \x20   afterFor(emptyList())\n\
             \x20   return \"OK\"\n\
             }}\n"
        ),
        "throwing safe call after a loop",
    );
}
