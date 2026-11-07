//! A same-module inline function that is not public API is inlined from its compiled body when
//! another file calls it, as kotlinc compiles every same-module callee with its own file. The body
//! reaches its file's private declarations through the accessors its own class publishes, and a
//! member's inline-frame marker carries the module-mangled JVM name of the internal method.

use super::common;

const DECLARATION: &str = r#"package lib

private fun twice(x: Int) = x * 2

internal inline fun <T> same(value: T): T = value

internal inline fun doubled(x: Int, f: (Int) -> Int): Int = f(twice(x))

class Counter private constructor(val start: Int) {
    private fun next() = start + 1

    internal inline fun nextVia(f: (Int) -> String): String = f(next())

    companion object {
        internal inline fun create(start: Int) = Counter(start)
    }
}
"#;

const CALLER: &str = r#"package lib

fun box(): String {
    if (same(7) != 7) return "same int"
    if (same("s") != "s") return "same string"
    if (doubled(3) { it + 1 } != 7) return "doubled"
    return Counter.create(1).nextVia { if (it == 2) "OK" else "next $it" }
}
"#;

#[test]
fn an_internal_inline_body_of_another_file_is_inlined_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(&[
        ("Declaration.kt", DECLARATION),
        ("Caller.kt", CALLER),
    ]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn an_internal_inline_body_of_another_file_runs() {
    common::expect_box_ok_files_with_stdlib(
        &[("Declaration.kt", DECLARATION), ("Caller.kt", CALLER)],
        "Caller",
    );
}
