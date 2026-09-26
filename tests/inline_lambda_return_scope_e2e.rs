//! A `return` may leave a lambda only when the lambda is inlined into the frame around it: an
//! argument for a parameter of an inline function that wrote neither `crossinline` nor `noinline`.
//! Every other lambda between the `return` and its target makes kotlinc report
//! `'return' is prohibited here.` at the `return`. A labelled return to the lambda itself stays
//! legal whatever the parameter wrote. A function value's `invoke` is no inline function, whether
//! the value is a parameter or a property, while an inline `invoke` operator inlines like any call.

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
    inline fun spliced(f: (Int) -> Int): Int = f(v)
}
inline fun Int.shifted(crossinline f: (Int) -> Int): Int = f(this)
class Holder(val compute: (() -> Int) -> Int)
class Runner {
    inline operator fun invoke(f: () -> Int): Int = f()
}
class Tag
inline operator fun Tag.invoke(f: () -> Int): Int = f()
class Wrapper(val runner: Runner, val tag: Tag)
class OverloadedRunner {
    inline operator fun invoke(marker: Int = 0, f: (String) -> Int): Int = f(\"inline\") + marker
    operator fun invoke(marker: String = \"\", f: (Int) -> Int): Int = f(marker.length)
}
class OverloadedHolder(val runner: OverloadedRunner)
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
fun throughFunctionValue(compute: (() -> Int) -> Int): Int = compute { return 10 }
fun throughProperty(holder: Holder): Int = holder.compute { return 11 }
fun throughInlineInvoke(runner: Runner): Int = runner { return 12 }
fun throughInlineInvokeProperty(wrapper: Wrapper): Int = wrapper.runner { return 13 }
fun throughExtensionInvokeProperty(wrapper: Wrapper): Int = wrapper.tag { return 14 }
fun memberSpliced(): Int = Box(1).spliced { return 15 }
fun throughSelectedOverload(holder: OverloadedHolder): Int = holder.runner { _: Int -> return 16 }
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
        (13, 72),
        (14, 61),
        (19, 88),
    ]
    .into_iter()
    .map(|(line, column)| prohibited(file, line, column))
    .collect()
}

#[test]
fn missing_inline_parameter_metadata_never_grants_return_permission() {
    let shape = krusty::symbol_resolver::LambdaCallShape {
        inline: true,
        boxes_captures: Some(vec![None]),
        ..krusty::symbol_resolver::LambdaCallShape::default()
    };
    assert_eq!(shape.inlines_argument(0), None);
    assert_eq!(shape.inlines_argument(1), None);
}

#[test]
fn a_property_invoke_plan_keeps_one_overload_for_shaping_and_recording() {
    const SOURCE: &str = "\
class RoutedCall {
    inline operator fun invoke(marker: Int = 0, block: (Int) -> String): String = \"integer:\" + block(marker)
    operator fun invoke(marker: String = \"text\", block: (String) -> String): String = \"string:\" + block(marker)
}
class RoutedHolder(val call: RoutedCall)
class ScopedCall
class ScopedHolder(val call: ScopedCall)
class CallScope {
    inline operator fun ScopedCall.invoke(marker: Int = 0, block: (Int) -> String): String = \"scoped-integer:\" + block(marker)
    operator fun ScopedCall.invoke(marker: String = \"text\", block: (String) -> String): String = \"scoped-string:\" + block(marker)
    fun route(holder: ScopedHolder): String {
        val integer = holder.call { value: Int -> value.toString() }
        val string = holder.call { value: String -> value }
        return \"$integer/$string\"
    }
}
fun box(): String {
    val holder = RoutedHolder(RoutedCall())
    val integer = holder.call { value: Int -> value.toString() }
    val string = holder.call { value: String -> value }
    val scoped = CallScope().route(ScopedHolder(ScopedCall()))
    return if (integer == \"integer:0\" && string == \"string:text\" && scoped == \"scoped-integer:0/scoped-string:text\") \"OK\" else \"$integer/$string/$scoped\"
}
";
    assert_eq!(
        common::compile_and_run_with_stdlib(SOURCE, "SelectedInvokePlan").as_deref(),
        Some("OK")
    );
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
