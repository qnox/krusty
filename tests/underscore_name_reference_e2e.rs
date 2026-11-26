//! A name made only of underscores is reserved: used as a reference or call callee without
//! back-ticks it is an error, while the positions where `_` is syntax (an unused lambda parameter,
//! a destructuring component) and a back-ticked name stay valid. Diagnostics are compared with
//! kotlinc's, including their position and order.

use super::common;

const INVALID: &str = "val `_` = 1\n\
fun `_`() = 1\n\
fun `__`() = 2\n\
fun <T> `___`() = 3\n\
fun use(value: Int) = value\n\
fun reference() = use(_)\n\
fun longer() = use(__())\n\
fun callee() = _()\n\
fun generic() = ___<Int>()\n\
fun unresolved() = use(____)\n";

const VALID: &str = "fun `_`() = 1\n\
fun lambda(): (Int, Int) -> Int = { _, b -> b }\n\
fun destructure(pair: Pair<Int, Int>): Int {\n\
    val (_, second) = pair\n\
    return second + `_`()\n\
}\n";

#[test]
fn an_underscore_only_name_reference_without_back_ticks_is_rejected_like_kotlinc() {
    let result = common::compiler_diagnostics(&[("Invalid.kt", INVALID)], &[common::stdlib_jar()]);
    let reference = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(result.reference_code, 1, "{}", result.reference_stderr);
    assert_eq!(result.krusty_code, 1, "{}", result.krusty_stderr);
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        reference
    );
    assert_eq!(
        reference,
        [
            "Invalid.kt:6:23: names _, __, ___, ... can only be used in back-ticks (`_`, `__`, `___`, ...).",
            "Invalid.kt:7:20: names _, __, ___, ... can only be used in back-ticks (`_`, `__`, `___`, ...).",
            "Invalid.kt:8:16: names _, __, ___, ... can only be used in back-ticks (`_`, `__`, `___`, ...).",
            "Invalid.kt:9:17: names _, __, ___, ... can only be used in back-ticks (`_`, `__`, `___`, ...).",
            "Invalid.kt:10:24: unresolved reference '____'.",
        ]
    );
}

#[test]
fn underscore_syntax_and_back_ticked_names_stay_valid() {
    let result = common::compiler_diagnostics(&[("Valid.kt", VALID)], &[common::stdlib_jar()]);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(result.krusty_code, 0, "{}", result.krusty_stderr);
}
