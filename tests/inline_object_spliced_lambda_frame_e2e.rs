//! A crossinline lambda an inline function's anonymous object invokes is spliced into each call's
//! copy of the object. kotlinc opens the spliced body with the lambda's inline-depth marker
//! (`$i$a$-wrap-…Kt$box$1`) and maps its lines through the call site, as it does for a lambda
//! inlined at an ordinary call. Both copies are byte-identical to kotlinc's.

use super::common;

const PROGRAM: &str = include_str!("box_regressions/inlineObjectSplicedLambdaFrame.kt");

#[test]
fn a_lambda_spliced_into_an_object_copy_opens_its_own_frame() {
    assert_eq!(
        common::expect_box_run_with_stdlib(PROGRAM, "InlineObjectSplicedLambdaFrame"),
        "OK"
    );
    for class in [
        "InlineObjectSplicedLambdaFrameKt$box$$inlined$wrap$1",
        "InlineObjectSplicedLambdaFrameKt$box$$inlined$wrap$2",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "InlineObjectSplicedLambdaFrame",
            PROGRAM,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
