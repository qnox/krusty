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

/// An anonymous function with a declared result type keeps it: `fun(): Any = ...` is an `Any`
/// regardless of the intersection its body proves, so a `Styled` requirement rejects it as kotlinc
/// does.
#[test]
fn a_declared_anonymous_function_result_keeps_its_type() {
    use super::diagnostics_parity_support::{errors, ObservedError};

    let source = "\
interface Styled
interface Backed
class Paragraph : Styled, Backed
class Link : Styled, Backed
fun <R> compute(block: () -> R): R = block()
fun requireStyled(s: Styled) {}
fun f(w: Boolean) {
    requireStyled(compute(fun(): Any = if (w) Paragraph() else Link()))
}
";
    let result = common::compiler_diagnostics(&[("Anon.kt", source)], &[common::stdlib_jar()]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    // Both reject; the two compilers word and anchor the rejection differently.
    assert_eq!(
        krusty,
        vec![ObservedError {
            file: "Anon.kt".to_string(),
            line: 8,
            column: 40,
            message: "return type mismatch: expected 'Styled', actual 'Any'.".to_string(),
        }]
    );
    assert_eq!(
        errors(&result.reference_stderr),
        vec![ObservedError {
            file: "Anon.kt".to_string(),
            line: 8,
            column: 19,
            message: "argument type mismatch: actual type is 'Any', but 'Styled' was expected."
                .to_string(),
        }]
    );
}

const LOCAL_SOURCE: &str = include_str!("box_regressions/LocalIntersectionArgument.kt");

/// A local whose value is a non-denotable intersection reaches a generic call with its
/// constituents, whether the intersection comes from its initializer or from flow narrowing, for a
/// plain parameter and for a vararg element ranked against a single-element overload. The program
/// runs on every backend from the shared box inventory; this driver compares its result with
/// kotlinc.
#[test]
fn a_local_intersection_reaches_the_generic_call_result() {
    common::expect_box_same_as_kotlinc(LOCAL_SOURCE, "LocalIntersectionArgumentRun");
}

/// The vararg collection factory takes the same flow-narrowed intersection.
///
/// PLATFORM-SPECIFIC: this is the quarkdown shape with the real `listOf`, whose KLIB body the
/// wasm lanes cannot yet lower. The same overload shapes (a single-element overload beside a
/// vararg one) run on every backend in `LocalIntersectionArgument.kt`, which is not cropped.
#[test]
fn a_local_intersection_reaches_a_vararg_collection() {
    const PRELUDE: &str = "\
interface Node
interface Styled : Node
interface Backed : Node
class Paragraph : Styled, Backed
class Link : Styled, Backed
class Items : Node
fun render(nodes: List<Node>): Int = nodes.size
";
    for (name, body, expected) in [
        (
            "Initializer",
            "fun run(): Int { val x = if (true) Paragraph() else Link(); return render(listOf(x, Items())) }",
            2,
        ),
        (
            "Narrowing",
            "fun run(): Int { val v: Any = Paragraph(); if (v is Styled && v is Backed) return render(listOf(v, Items())); return 0 }",
            2,
        ),
        (
            "Single",
            "fun run(): Int { val v: Any = Paragraph(); if (v is Styled && v is Backed) return render(listOf(v)); return 0 }",
            1,
        ),
    ] {
        let source = format!(
            "{PRELUDE}{body}\nfun box(): String = if (run() == {expected}) \"OK\" else \"Fail\"\n"
        );
        common::expect_box_same_as_kotlinc(&source, &format!("LocalIntersectionVararg{name}Run"));
    }
}
