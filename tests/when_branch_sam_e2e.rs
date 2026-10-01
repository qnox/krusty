//! A `when` or `if` whose expected type is a fun interface converts the lambda each branch yields.
//!
//! The branch may be the lambda itself (`{ text -> text.n }`) or a block whose value is one
//! (`{ { it.n } }`). Either way the value that leaves the conditional implements the interface.
//! Leaving the function object and check-casting it throws `ClassCastException`.

use super::common;

const SOURCE: &str = "\
class Text(val n: Int)\n\
fun interface Read {\n\
    fun read(text: Text): Int\n\
}\n\
fun box(): String {\n\
    val direct: Read = when {\n\
        true -> { text -> text.n }\n\
        else -> { text -> text.n }\n\
    }\n\
    if (direct.read(Text(2)) != 2) return \"fail direct\"\n\
    val blocked: Read = when {\n\
        true -> {\n\
            { it.n }\n\
        }\n\
        else -> {\n\
            { it.n }\n\
        }\n\
    }\n\
    if (blocked.read(Text(2)) != 2) return \"fail blocked\"\n\
    val viaIf: Read = if (true) {\n\
        { it.n }\n\
    } else {\n\
        { it.n }\n\
    }\n\
    if (viaIf.read(Text(2)) != 2) return \"fail if\"\n\
    val nested: Read = if (true) {\n\
        if (true) { { it.n } } else { { it.n } }\n\
    } else {\n\
        { text -> text.n }\n\
    }\n\
    if (nested.read(Text(2)) != 2) return \"fail nested\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn a_conditional_branch_lambda_converts_to_the_expected_fun_interface() {
    assert_eq!(common::kotlinc_box_result(SOURCE), "OK");
    assert_eq!(
        common::expect_box_run_with_stdlib(SOURCE, "WhenBranchSam"),
        "OK"
    );
}
