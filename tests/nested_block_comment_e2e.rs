//! Kotlin block comments nest: a `/*` inside a comment opens another level that its own `*/`
//! closes, so a KDoc quoting `/* */` stays one comment. A comment still open at the end of the
//! file is kotlinc's `Unclosed comment` syntax error, reported at the end of the file.

use super::common;

#[test]
fn a_comment_inside_a_kdoc_or_block_comment_does_not_end_it() {
    common::expect_box_same_as_kotlinc(
        "class Query {
    /**
     * Appends a comment wrapped by `/* */`, like the `/*FORCE_MASTER*/` hint.
     */
    fun comment(): String = \"O\"
}
/* outer /* inner */ still a comment */ val k = \"K\"
fun box(): String = Query().comment() + k
",
        "NestedBlockComment",
    );
}

/// The inner `*/` closes only the nested level, so the outer comment runs to the end of the file.
#[test]
fn an_unclosed_nested_comment_is_kotlincs_syntax_error() {
    let result = common::compiler_diagnostics(
        &[("Unclosed.kt", "fun box() = \"OK\"\n/* outer /* inner */ ^")],
        &[common::stdlib_jar()],
    );
    common::expect_identical_rejection(&result, "unclosed_nested_comment");
}

/// The error sits at the end of the file, after any trailing newlines.
#[test]
fn an_unclosed_comment_is_reported_at_the_end_of_the_file() {
    let result = common::compiler_diagnostics(
        &[("Trailing.kt", "fun box() = \"OK\"\n/* x\n\n")],
        &[common::stdlib_jar()],
    );
    common::expect_identical_rejection(&result, "unclosed_comment_at_eof");
}
