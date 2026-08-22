//! Expression-parser completeness, compiled and executed under `-Xverify:all`.

use super::common;

/// Strict stdlib/JDK run: missing tooling or a rejected source panics with diagnostics.
fn run(src: &str) -> String {
    common::expect_box_run_with_stdlib(src, "P")
}

fn assert_frontends_accept(source: &str, stem: &str) {
    assert_eq!(
        common::kotlinc_source_result(stem, source),
        (0, String::new())
    );
    assert_eq!(
        common::front_end_diagnostics_with_stdlib(source),
        Vec::<String>::new()
    );
}

#[test]
fn unary_plus_runs() {
    const SRC: &str = "fun box(): String {\n\
        val a = +5\n\
        val b = +0.0f\n\
        if (a != 5) return \"fail a\"\n\
        if (0.compareTo(b) != 0) return \"fail b\"\n\
        return \"OK\"\n\
    }\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn return_in_expression_position_runs() {
    const SRC: &str = "fun firstOrNull(x: Int?): Int {\n\
        val v = x ?: return -1\n\
        return v + 1\n\
    }\n\
    fun box(): String {\n\
        if (firstOrNull(null) != -1) return \"fail null\"\n\
        if (firstOrNull(10) != 11) return \"fail val\"\n\
        return \"OK\"\n\
    }\n";
    assert_eq!(run(SRC), "OK");
}

#[test]
fn throw_operand_includes_the_elvis_chain() {
    const SOURCE: &str = r#"
fun rethrow(error: IllegalStateException): Nothing =
    try {
        throw error
    } catch (caught: Exception) {
        throw caught.cause ?: caught
    }

fun box(): String {
    val error = IllegalStateException("outer")
    return try {
        rethrow(error)
    } catch (caught: Throwable) {
        if (caught === error) "OK" else "FAIL"
    }
}
"#;
    assert_frontends_accept(SOURCE, "ThrowElvisOperand");
    assert_eq!(run(SOURCE), "OK");
}

#[test]
fn expression_position_return_operand_includes_the_elvis_chain() {
    const SOURCE: &str = r#"
fun choose(first: String?, second: String?): String {
    first ?: return second ?: "fallback"
    return first
}

fun box(): String =
    if (
        choose(null, null) == "fallback" &&
        choose(null, "second") == "second" &&
        choose("first", null) == "first"
    ) "OK" else "FAIL"
"#;
    assert_frontends_accept(SOURCE, "ReturnElvisOperand");
    assert_eq!(run(SOURCE), "OK");
}
