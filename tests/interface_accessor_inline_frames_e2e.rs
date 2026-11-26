//! An interface member property takes no place in its interface's initialization order, but the
//! naming walk still visits its accessors: a lambda spliced into a getter or setter body opens its
//! `$i$a$-<callee>-<lambda class>` frame, named after the class the lambda would compile to. The
//! interface's default accessors and their `DefaultImpls` bridges are byte-identical to kotlinc's.

use super::common;

const PROGRAM: &str = include_str!("box_regressions/interfaceAccessorSplicedLambdaFrames.kt");

#[test]
fn interface_accessor_lambda_frames_are_the_reference_compilers() {
    assert_eq!(
        common::expect_box_run_with_stdlib(PROGRAM, "InterfaceAccessorFrames"),
        "OK"
    );
    for class in ["FieldSet", "FieldSet$DefaultImpls"] {
        common::byte_diff_against_kotlinc_cp(
            "InterfaceAccessorFrames",
            PROGRAM,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
