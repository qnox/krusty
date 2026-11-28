//! `tailrec` warnings: the reference compiler's `FirTailrecFunctionChecker`.
//!
//! Every fixture compiles with both compilers; krusty must report exactly the warnings kotlinc
//! reports, in the same order and at the same positions, and no errors. Each fixture groups one
//! family of source forms; the list beside it is kotlinc's complete output for that file.

use super::common;
use krusty::kotlin_version::{Quirk, Quirks};

/// The lists beside the fixtures are kotlinc 2.5's output. Earlier releases differ in the
/// positions of dotless calls, in `return Unit` and in `?:`, so there kotlinc's own output on the
/// same source is the only expectation.
fn is_listed_release() -> bool {
    let release = Quirks::of_release(krusty::kotlin_version::environment_release());
    ![
        Quirk::TailrecDotlessCallReportedAtOperator,
        Quirk::TailrecUnitObjectReturnFollowsCall,
        Quirk::TailrecElvisLeftOperandIsTail,
    ]
    .into_iter()
    .any(|quirk| release.has(quirk))
}

#[derive(Clone, Copy)]
enum Warning {
    /// `a function is marked as tail-recursive but no tail calls are found.`
    NoTailCalls,
    /// `recursive call is not a tail call.`
    NonTail,
    /// `tail recursion optimization inside try/catch/finally is not supported.`
    InTry,
}

impl Warning {
    fn message(self) -> &'static str {
        match self {
            Self::NoTailCalls => {
                "a function is marked as tail-recursive but no tail calls are found."
            }
            Self::NonTail => "recursive call is not a tail call.",
            Self::InTry => "tail recursion optimization inside try/catch/finally is not supported.",
        }
    }
}

fn expected(file: &str, warnings: &[(usize, usize, Warning)]) -> Vec<common::CompilerError> {
    warnings
        .iter()
        .map(|&(line, column, warning)| common::CompilerError {
            file: file.to_string(),
            line,
            column,
            message: warning.message().to_string(),
        })
        .collect()
}

fn assert_warnings(file: &str, source: &str, warnings: &[(usize, usize, Warning)]) {
    let expected = expected(file, warnings);
    let result = common::compiler_diagnostics_with_shared_args(
        &[(file, source)],
        &[common::stdlib_jar()],
        &[],
    );
    assert_eq!(common::compiler_errors(&result.reference_stderr), []);
    if is_listed_release() {
        assert_eq!(
            common::compiler_warnings(&result.reference_stderr),
            expected
        );
    }
    assert_eq!(common::compiler_errors(&result.krusty_stderr), []);
    assert_eq!(
        common::compiler_warnings(&result.krusty_stderr),
        common::compiler_warnings(&result.reference_stderr)
    );
    assert_eq!(result.krusty_code, result.reference_code);
}

#[test]
fn valid_forms_report_nothing() {
    assert_warnings(
        "Valid.kt",
        include_str!("tailrec_diagnostics_e2e/Valid.kt"),
        &[],
    );
}

#[test]
fn calls_with_instructions_after_them_are_not_tail_calls() {
    assert_warnings(
        "NotTail.kt",
        include_str!("tailrec_diagnostics_e2e/NotTail.kt"),
        &[
            (1, 1, Warning::NoTailCalls),
            (1, 42, Warning::NonTail),
            (2, 1, Warning::NoTailCalls),
            (2, 57, Warning::NonTail),
            (3, 1, Warning::NoTailCalls),
            (3, 52, Warning::NonTail),
            (4, 50, Warning::NonTail),
            (5, 1, Warning::NoTailCalls),
            (5, 71, Warning::NonTail),
            (6, 1, Warning::NoTailCalls),
            (6, 37, Warning::NonTail),
            (7, 1, Warning::NoTailCalls),
            (7, 56, Warning::NonTail),
            (8, 1, Warning::NoTailCalls),
            (8, 54, Warning::NonTail),
            (9, 1, Warning::NoTailCalls),
            (9, 56, Warning::NonTail),
            (13, 29, Warning::NonTail),
            (17, 1, Warning::NoTailCalls),
            (17, 45, Warning::NonTail),
            (18, 1, Warning::NoTailCalls),
            (20, 1, Warning::NoTailCalls),
            (20, 60, Warning::NonTail),
            (21, 1, Warning::NoTailCalls),
            (23, 19, Warning::NonTail),
            (26, 1, Warning::NoTailCalls),
            (26, 52, Warning::NonTail),
            (28, 1, Warning::NoTailCalls),
            (30, 17, Warning::NonTail),
            (32, 1, Warning::NoTailCalls),
            (32, 69, Warning::NonTail),
            (33, 1, Warning::NoTailCalls),
            (34, 24, Warning::NonTail),
            (37, 1, Warning::NoTailCalls),
        ],
    );
}

#[test]
fn calls_in_try_catch_and_finally_are_not_supported() {
    assert_warnings(
        "Try.kt",
        include_str!("tailrec_diagnostics_e2e/Try.kt"),
        &[
            (1, 1, Warning::NoTailCalls),
            (1, 52, Warning::InTry),
            (2, 1, Warning::NoTailCalls),
            (2, 79, Warning::InTry),
            (2, 116, Warning::InTry),
            (3, 1, Warning::NoTailCalls),
            (3, 59, Warning::InTry),
        ],
    );
}

#[test]
fn calls_on_another_receiver_or_omitting_arguments_are_not_tail_calls() {
    assert_warnings(
        "Receivers.kt",
        include_str!("tailrec_diagnostics_e2e/Receivers.kt"),
        &[
            (3, 5, Warning::NoTailCalls),
            (3, 83, Warning::NonTail),
            (4, 5, Warning::NoTailCalls),
            (4, 81, Warning::NonTail),
            (5, 5, Warning::NoTailCalls),
            (5, 96, Warning::NonTail),
            (6, 5, Warning::NoTailCalls),
            (9, 36, Warning::NoTailCalls),
            (9, 105, Warning::NonTail),
            (14, 14, Warning::NoTailCalls),
            (14, 75, Warning::NonTail),
        ],
    );
}

#[test]
fn nested_functions_classes_and_dead_code() {
    assert_warnings(
        "Nested.kt",
        include_str!("tailrec_diagnostics_e2e/Nested.kt"),
        &[
            (3, 5, Warning::NoTailCalls),
            (3, 49, Warning::NonTail),
            (4, 5, Warning::NoTailCalls),
            (4, 54, Warning::InTry),
            (5, 5, Warning::NoTailCalls),
            (9, 5, Warning::NoTailCalls),
            (9, 78, Warning::NonTail),
            (10, 5, Warning::NoTailCalls),
            (10, 65, Warning::NonTail),
            (12, 1, Warning::NoTailCalls),
            (16, 1, Warning::NoTailCalls),
            (17, 1, Warning::NoTailCalls),
            (18, 1, Warning::NoTailCalls),
        ],
    );
}

#[test]
fn infix_and_operator_calls_are_reported_at_their_start() {
    assert_warnings(
        "Operators.kt",
        include_str!("tailrec_diagnostics_e2e/Operators.kt"),
        &[
            (1, 7, Warning::NoTailCalls),
            (1, 53, Warning::NonTail),
            (2, 7, Warning::NoTailCalls),
            (2, 62, Warning::NonTail),
            (3, 7, Warning::NoTailCalls),
            (3, 60, Warning::NonTail),
            (5, 5, Warning::NoTailCalls),
            (5, 54, Warning::NonTail),
            (5, 68, Warning::NonTail),
            (6, 5, Warning::NoTailCalls),
            (6, 57, Warning::NonTail),
            (8, 1, Warning::NoTailCalls),
            (8, 41, Warning::NonTail),
            (8, 58, Warning::NonTail),
            (9, 1, Warning::NoTailCalls),
            (9, 42, Warning::NonTail),
        ],
    );
}
