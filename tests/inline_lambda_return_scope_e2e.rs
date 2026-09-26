//! A `return` may leave a lambda only when the lambda is inlined into the frame around it: an
//! argument for a parameter of an inline function that wrote neither `crossinline` nor `noinline`.
//! Every other lambda between the `return` and its target makes kotlinc report
//! `'return' is prohibited here.` at the `return`. A labelled return to the lambda itself stays
//! legal whatever the parameter wrote.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

const PROHIBITED: &str = "'return' is prohibited here.";

const DECLARATIONS: &str = "\
inline fun applied(x: Int, crossinline f: (Int) -> Int): Int = f(x)
inline fun kept(x: Int, noinline f: (Int) -> Int): Int = f(x)
inline fun plain(x: Int, f: (Int) -> Int): Int = f(x)
fun stored(f: () -> Int): Int = f()
class Box(val v: Int) {
    inline fun mapped(crossinline f: (Int) -> Int): Int = f(v)
    inline fun passed(noinline f: (Int) -> Int): Int = f(v)
}
inline fun Int.shifted(crossinline f: (Int) -> Int): Int = f(this)
";

/// The uses, from line 1 of their own file.
const USES: &str = "\
fun crossinlined(): Int = applied(1) { return 2 }
fun noinlined(): Int = kept(1) { return 2 }
fun labelled(): Int = applied(1) { return@applied 2 } + kept(1) { return@kept 3 }
fun spliced(): Int = plain(1) { return 2 }
fun nestedInCrossinline(): Int = plain(1) { applied(it) { return 3 } }
fun throughStored(): Int = applied(1) { val g = { y: Int -> return@applied y }; g(2) }
fun notInline(): Int = stored { return 4 }
fun memberCrossinline(): Int = Box(1).mapped { return 5 }
fun memberNoinline(): Int = Box(1).passed { return 6 }
fun extensionCrossinline(): Int = 1.shifted { return 7 }
fun namedCrossinline(): Int = 1.shifted(f = { return 8 })
fun memberLabelled(): Int = Box(1).mapped { return@mapped 9 }
";

fn prohibited(file: &str, line: usize, column: usize) -> ObservedError {
    ObservedError {
        file: file.to_string(),
        line,
        column,
        message: PROHIBITED.to_string(),
    }
}

fn expected(file: &str) -> Vec<ObservedError> {
    [
        (1, 40),
        (2, 34),
        (5, 59),
        (6, 61),
        (7, 33),
        (8, 48),
        (9, 45),
        (10, 47),
        (11, 47),
    ]
    .into_iter()
    .map(|(line, column)| prohibited(file, line, column))
    .collect()
}

fn assert_prohibited_returns(result: common::CompilerDiagnosticResult, file: &str) {
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    let reference = errors(&result.reference_stderr);
    assert_eq!(
        reference,
        expected(file),
        "kotlinc: {}",
        result.reference_stderr
    );
    assert_eq!(krusty, reference, "krusty: {}", result.krusty_stderr);
}

#[test]
fn returns_leaving_non_inlined_lambdas_are_prohibited_for_same_module_callees() {
    let result = common::compiler_diagnostics(
        &[("Declarations.kt", DECLARATIONS), ("Uses.kt", USES)],
        &[common::stdlib_jar()],
    );
    assert_prohibited_returns(result, "Uses.kt");
}

#[test]
fn returns_leaving_non_inlined_lambdas_are_prohibited_for_classpath_callees() {
    let library = common::kotlinc_lib_out(&[("Declarations.kt", DECLARATIONS)])
        .expect("reference kotlinc is provisioned");
    let result =
        common::compiler_diagnostics(&[("Uses.kt", USES)], &[library, common::stdlib_jar()]);
    assert_prohibited_returns(result, "Uses.kt");
}
