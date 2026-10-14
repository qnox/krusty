//! A library scope function lowered from its declaration plan (`run`, `let`, and owned shapes
//! that return the lambda's result or their receiver) keeps the call site kotlinc's inliner
//! writes: the opening `nop` on the call line when no operand already carries that line, the
//! `$i$f$`/`$i$a$` markers with their local-variable rows, and the fake.kt SMAP stratum that maps
//! the inlined lines, byte-for-byte.

use super::common;

#[test]
fn a_run_call_is_inlined_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Run.kt",
        r#"
fun g(): Int = run { 1 }
"#,
    )]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn a_let_call_with_a_captured_value_is_inlined_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Let.kt",
        r#"
fun shifted(value: Int, offset: Int): Int = value.let { it + offset }
"#,
    )]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

const LIB: &str = r#"
inline fun <T, R> T.convert(block: (T) -> R): R = block(this)

inline fun <T> T.observe(block: (T) -> Unit): T {
    block(this)
    return this
}
"#;

const MAIN: &str = r#"
var seen = 0

fun converted(value: Int, offset: Int): Int = value.convert { it + offset }

fun observed(value: String): String = value.observe { seen = it.length }

fun box(): String {
    val converted = converted(40, 2)
    val observed = observed("OK")
    return if (converted == 42 && seen == 2) observed else "FAIL $converted $seen"
}
"#;

#[test]
fn owned_scope_shapes_run() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}

#[test]
fn owned_scope_shapes_are_inlined_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_lib("Main", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// The method after a top-level setter is rewritten (the inlined `let` drops its opening `nop`).
/// The setter's `<set-?>` local name stays with the setter in the constant pool, ahead of that
/// method's entries, where kotlinc's writer interns it.
#[test]
fn a_setter_local_name_keeps_its_place_before_a_rewritten_inline_call() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Setter.kt",
        r#"
var seen = 0

fun converted(value: Int, offset: Int): Int = value.let { it + offset }
"#,
    )]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// A statement-position `with`/`run` whose lambda is a multi-line `Unit` body. After the lambda's
/// closing-brace `nop`, while its locals are open, kotlinc's inliner returns to the call's line on
/// a `nop` of its own once the lambda locals end. The redundant-`nop` sweep keeps it: nothing else
/// is on that line before the next statement's.
#[test]
fn a_statement_unit_lambda_returns_to_the_call_line_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_module(&[(
        "Statement.kt",
        r#"
var log = ""

fun record(text: String) {
    log += text
}

fun box(): String {
    with("O") {
        record(this)
        record(length.toString())
    }
    run {
        record("K")
        record("!")
    }
    return log
}
"#,
    )]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
