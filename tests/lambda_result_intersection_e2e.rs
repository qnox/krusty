//! A lambda whose result is a non-denotable intersection (`if (c) Paragraph() else it`, two classes
//! that share several interfaces) passed to a generic call keeps that intersection as the call's
//! result, so the expected `Pair2<Node>` is checked against its components rather than the `Any`
//! its denotable view collapses to. The program runs on every backend from the shared box
//! inventory; this driver compares its result with kotlinc.
use super::common;

const SOURCE: &str = include_str!("box_regressions/LambdaResultIntersectionArgument.kt");

#[test]
fn a_lambda_result_intersection_reaches_the_generic_call_result() {
    common::expect_box_same_as_kotlinc(SOURCE, "LambdaResultIntersectionArgumentRun");
}

/// An invariant sibling (`Cell<Any>`) fixes the result variable to `Any`; the lambda's own join of
/// two classes does not widen the call to claim it is `Styled`. Both compilers reject passing the
/// result where a `Styled` is required, with the same diagnostic at the same position.
#[test]
fn an_invariant_sibling_keeps_the_denotable_result() {
    use super::diagnostics_parity_support::{errors, ObservedError};

    let source = "\
interface Styled
interface Backed
class Paragraph : Styled, Backed
class Link : Styled, Backed
class Cell<T>(val value: T)
fun <R> keep(cell: Cell<R>, block: () -> R): R = cell.value
fun requireStyled(s: Styled) {}
fun f(w: Boolean) {
    requireStyled(keep(Cell<Any>(\"text\")) { if (w) Paragraph() else Link() })
}
";
    let result = common::compiler_diagnostics(&[("Keep.kt", source)], &[common::stdlib_jar()]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let expected = vec![ObservedError {
        file: "Keep.kt".to_string(),
        line: 9,
        column: 19,
        message: "argument type mismatch: actual type is 'Any', but 'Styled' was expected."
            .to_string(),
    }];
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}
