//! Extension receivers on a value a smart cast proved to be two incomparable types: kotlinc ranks
//! the intersection's constituents at one rung, so an extension declared on each of them is an
//! ambiguity rather than a choice by the order the types were proved in.

use super::common;

const FILE: &str = "Main.kt";

#[test]
fn extensions_on_two_proved_constituents_are_ambiguous() {
    let source = "interface A\n\
                  interface B\n\
                  fun A.cast(): String = \"A\"\n\
                  fun B.cast(): String = \"B\"\n\
                  fun f(x: A): String = if (x is B) x.cast() else \"none\"\n";
    let result = common::compiler_diagnostics_with_shared_args(
        &[(FILE, source)],
        &[common::stdlib_jar()],
        &[],
    );
    let expected = vec![common::CompilerError {
        file: FILE.to_string(),
        line: 5,
        column: 37,
        message: "overload resolution ambiguity between candidates:".to_string(),
    }];
    assert_eq!(common::compiler_errors(&result.reference_stderr), expected);
    assert_eq!(common::compiler_errors(&result.krusty_stderr), expected);
    assert_eq!(common::compiler_warnings(&result.reference_stderr), []);
    assert_eq!(common::compiler_warnings(&result.krusty_stderr), []);
    assert_ne!(result.reference_code, 0);
    assert_ne!(result.krusty_code, 0);
}

#[test]
fn a_bare_formal_receiver_binds_the_whole_intersection() {
    common::expect_box_ok_with_stdlib(
        include_str!("box_regressions/smartCastIntersectionBareFormalReceiver.kt"),
        "SmartCastIntersectionBareFormalReceiver",
    );
}
