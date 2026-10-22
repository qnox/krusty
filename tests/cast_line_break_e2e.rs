//! Kotlin's grammar allows line breaks on either side of a cast (`NL* asOperator NL* type`), so a
//! call chain can end in `as? T` on its own line. Now in Android's `DelegatingWorker` writes
//! `.createWorker(...)` and then `as? CoroutineWorker` and `?: throw` on the following lines.

use super::common;

#[test]
fn a_cast_continues_across_line_breaks_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Casts.kt",
        "package casts\n\
         \n\
         fun safe(value: Any): String =\n\
         \x20   value\n\
         \x20       as? String\n\
         \x20       ?: \"none\"\n\
         \n\
         fun plain(x: Any): String = x\n\
         \x20   as String\n\
         \n\
         fun afterOperator(x: Any): Int = x as\n\
         \x20   Int\n\
         \n\
         fun blankLines(x: Any): Int? = x as?\n\
         \n\
         \x20   Int\n\
         \n\
         fun statement(x: Any): Int {\n\
         \x20   val y = x\n\
         \n\
         \x20   as Int\n\
         \x20   return y\n\
         }\n",
    )]);
    let differences = classes.differences();
    assert!(
        differences.is_empty(),
        "classes differ from kotlinc's:\n\n{}",
        differences.join("\n\n")
    );
}
