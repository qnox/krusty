//! A binary operator's type parameter is seeded from the call's expected result.
//!
//! `fun pick(): List<P> = listOf(A()) + listOf(B())` was rejected:
//!
//! ```text
//! error: argument type mismatch: actual type is 'List<B>', but 'Iterable<A>' was expected.
//! ```
//!
//! `Iterable<T>.plus(Iterable<T>): List<T>` shares one `T` between the receiver, the argument and the
//! result. krusty fixed it from the RECEIVER alone — the receiver call had already been committed to
//! `List<A>` — so the argument was judged against `Iterable<A>`. Kotlin solves receiver, argument and
//! expected result together: the declared `List<P>` fixes `T = P`, and both operands then fit.
//!
//! Every spelling that pins `T` before the operator runs already worked — an explicit type argument
//! on the receiver call, or a declared `val` receiver — which is what isolates the expected result as
//! the missing input rather than the operands.

use super::common;
use std::rc::Rc;

/// Run one fixture under BOTH compilers and require the same `box()` value.
fn both_compilers_box(main: &str, stem: &str) {
    let reference = common::kotlinc_box_result(main);
    assert_eq!(
        reference, "OK",
        "{stem}: the reference compiler disagrees: {reference}"
    );
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", main)], stem);
}

const DECLARATIONS: &str = "interface P {\n\
\x20   val tag: String\n\
}\n\
\n\
class A : P {\n\
\x20   override val tag: String = \"a\"\n\
}\n\
\n\
class B : P {\n\
\x20   override val tag: String = \"b\"\n\
}\n\
\n";

/// The failing shape: both operands are calls whose element type is inferred, and only the declared
/// result relates them.
#[test]
fn a_declared_result_seeds_a_plus_over_two_inferred_operands() {
    let main = format!(
        "{DECLARATIONS}\
fun pick(): List<P> = listOf(A()) + listOf(B())\n\
fun box(): String {{\n\
\x20   val tags = pick().map {{ it.tag }}\n\
\x20   return if (tags == listOf(\"a\", \"b\")) \"OK\" else \"FAIL: \" + tags\n\
}}\n"
    );
    both_compilers_box(&main, "plus_declared_result");
}

/// The checked target must retain the SAME instantiation used to type the operand. Runtime success
/// alone does not prove that: `List<A>` is assignable to the declared covariant `List<P>`, so a
/// split decision can execute successfully while recording `T = P` for the parameter and `T = A`
/// for the result. Inspect the production common-IR boundary and require the plus call itself to
/// carry `List<P>`.
#[test]
fn the_recorded_plus_target_uses_the_expected_instantiation() {
    let main = format!(
        "{DECLARATIONS}\
fun pick(): List<P> = listOf(A()) + listOf(B())\n"
    );
    let cp = Rc::new(krusty::jvm::classpath::Classpath::new(vec![
        common::stdlib_jar(),
    ]));
    let (captured, diagnostics) = common::capture_common_ir(
        &main,
        "OperatorExpectedResult",
        Box::new(
            krusty::jvm::jvm_libraries::JvmLibraries::new(cp).expect("JVM provider initialization"),
        ),
    );
    assert_eq!(diagnostics, Vec::<String>::new());
    let [ir] = captured.as_slice() else {
        panic!("expected one common-IR file, got {}", captured.len());
    };
    let list = |element| krusty::types::Ty::obj_args("kotlin/collections/List", &[element]);
    let iterable = |element| krusty::types::Ty::obj_args("kotlin/collections/Iterable", &[element]);
    let a = krusty::types::Ty::obj("A");
    let b = krusty::types::Ty::obj("B");
    let p = krusty::types::Ty::obj("P");
    let external_calls = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            krusty::ir::IrExpr::Call {
                callee: krusty::ir::Callee::External { params, ret, .. },
                ..
            } => Some((params.clone(), *ret)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        external_calls,
        [
            (vec![a], list(a)),
            (vec![b], list(b)),
            (vec![iterable(p)], list(p)),
        ]
    );
}

/// The `fold` spelling the corpus actually uses: the accumulator is the receiver and the expected
/// result comes from the accumulator's own declared type.
#[test]
fn a_fold_accumulator_seeds_its_operator() {
    let main = format!(
        "{DECLARATIONS}\
fun merge(parts: List<A>): List<P> = parts.fold(listOf<P>()) {{ acc, part -> acc + listOf(part) }}\n\
fun box(): String {{\n\
\x20   val tags = merge(listOf(A(), A())).map {{ it.tag }}\n\
\x20   return if (tags == listOf(\"a\", \"a\")) \"OK\" else \"FAIL: \" + tags\n\
}}\n"
    );
    both_compilers_box(&main, "plus_fold_accumulator");
}

/// Control: an explicit type argument pins `T` before the operator runs, and always worked.
#[test]
fn an_explicit_type_argument_on_the_receiver_still_works() {
    let main = format!(
        "{DECLARATIONS}\
fun pick(): List<P> = listOf<P>(A()) + listOf(B())\n\
fun box(): String {{\n\
\x20   val tags = pick().map {{ it.tag }}\n\
\x20   return if (tags == listOf(\"a\", \"b\")) \"OK\" else \"FAIL: \" + tags\n\
}}\n"
    );
    both_compilers_box(&main, "plus_explicit_targ");
}

/// Control: a declared `val` receiver pins `T` the same way.
#[test]
fn a_declared_receiver_val_still_works() {
    let main = format!(
        "{DECLARATIONS}\
fun pick(): List<P> {{\n\
\x20   val first: List<P> = listOf(A())\n\
\x20   return first + listOf(B())\n\
}}\n\
fun box(): String {{\n\
\x20   val tags = pick().map {{ it.tag }}\n\
\x20   return if (tags == listOf(\"a\", \"b\")) \"OK\" else \"FAIL: \" + tags\n\
}}\n"
    );
    both_compilers_box(&main, "plus_declared_val");
}

/// Seeding from the expectation must not accept operands that genuinely do not fit: an `Int` element
/// has no common element type with the declared `List<P>` result.
#[test]
fn an_operand_that_does_not_fit_the_expectation_is_still_rejected() {
    let main = format!("{DECLARATIONS}fun pick(): List<P> = listOf(A()) + listOf(1)\n");
    let result = common::compiler_diagnostics(&[("Main.kt", &main)], &[]);
    // Both compilers reject. They place the blame differently, and the reason is the rule under
    // test: krusty pins `T = P` from the declared result and reports the operand that cannot meet
    // it, while kotlinc lets inference widen to `List<Any>` and reports the RETURN that no longer
    // matches. Recorded exactly rather than as a rejection check — a nonzero exit passes on an
    // unrelated rejection. The divergence is not introduced here: without the expectation krusty
    // reported the same argument mismatch against `Iterable<A>`, the receiver's element type.
    assert_eq!(
        common::compiler_errors(&result.krusty_stdout),
        [],
        "krusty writes diagnostics to stderr"
    );
    assert_eq!(
        common::compiler_errors(&result.krusty_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 13,
            column: 37,
            message: "argument type mismatch: actual type is 'List<Int>', but 'Iterable<P>' was \
                      expected."
                .to_string(),
        }]
    );
    assert_eq!(
        common::compiler_errors(&result.reference_stderr),
        [common::CompilerError {
            file: "Main.kt".to_string(),
            line: 13,
            column: 23,
            message: "return type mismatch: expected 'List<P>', actual 'List<Any>'.".to_string(),
        }]
    );
}
