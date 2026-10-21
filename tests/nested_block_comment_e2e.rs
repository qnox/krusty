//! Kotlin block comments nest: a `/*` inside a comment opens another level that its own `*/`
//! closes, so a KDoc quoting `/* */` stays one comment.

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
