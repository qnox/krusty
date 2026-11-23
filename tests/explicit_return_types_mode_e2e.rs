//! Explicit return types mode (`-XXexplicit-return-types=strict|warning`): the return-type half of
//! explicit API mode. Public API must write the types kotlinc would otherwise infer; visibility is
//! not checked. The Kotlin repository builds most of its compiler modules with
//! `-XXexplicit-return-types=warning` and `-Werror`.

use super::common;

const FIXTURE: &str = r#"package p

class A(val a: Int, c: Int) {
    constructor() : this(1, 2)
    companion object { val z = 1 }
    enum class E { X, Y; fun f() = 1 }
    protected fun prot() = 1
    internal fun inside() = 1
    private fun hidden() = 1
    fun block() { }
    fun typed(): Int = 1
    val withGetter get() = 1
    var withSetter: Int = 1
        set(value) { field = value }
    val lambda = { x: Int -> x }
    override fun toString() = "A"
    fun local() {
        fun inner() = 1
        val q = 2
        class Loc { fun m() = 1 }
    }
    private class Priv { fun m() = 1 }
}
data class D(val x: Int) { val y = 2; fun z() = 3 }
annotation class Q(val v: Int)
typealias T = Int
public class Pub { val anon = object { fun m() = 1 } }
enum class En(val v: Int) { A(1) { override fun f() = 2 }; open fun f() = 1 }
const val C = 1
val String.ext get() = length
fun <X> generic(t: X) = t
object Obj { fun m() = 1; private fun p() = 2 }
interface I { fun a(): Int; fun c() = 3 }
private fun privateTop() = 1
internal val internalTop = 2
"#;

fn flags(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

/// Strict mode reports each missing public return type as an error, exactly as kotlinc does.
#[test]
fn strict_explicit_return_types_report_like_kotlinc() {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("main.kt", FIXTURE)],
        &[],
        &flags(&["-XXexplicit-return-types=strict"]),
    );
    assert_eq!(result.reference_code, 1, "{}", result.reference_stderr);
    assert_eq!(result.krusty_code, 1, "{}", result.krusty_stderr);
    let reference = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(reference.len(), 15, "{}", result.reference_stderr);
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        reference
    );
    assert_eq!(common::compiler_warnings(&result.krusty_stderr), []);
    assert_eq!(common::compiler_warnings(&result.reference_stderr), []);
}

/// Warning mode reports the same declarations as warnings and the compilation succeeds.
#[test]
fn warning_explicit_return_types_report_like_kotlinc() {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("main.kt", FIXTURE)],
        &[],
        &flags(&["-XXexplicit-return-types=warning"]),
    );
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(result.krusty_code, 0, "{}", result.krusty_stderr);
    let reference = common::compiler_warnings(&result.reference_stderr);
    assert_eq!(reference.len(), 15, "{}", result.reference_stderr);
    assert_eq!(common::compiler_warnings(&result.krusty_stderr), reference);
}

/// With both flags at the same mode, explicit API mode's visibility and return-type checks run
/// once, as with `-Xexplicit-api` alone.
#[test]
fn matching_explicit_api_and_return_types_modes_report_like_kotlinc() {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("main.kt", FIXTURE)],
        &[],
        &flags(&["-Xexplicit-api=strict", "-XXexplicit-return-types=strict"]),
    );
    assert_eq!(result.reference_code, 1, "{}", result.reference_stderr);
    assert_eq!(result.krusty_code, 1, "{}", result.krusty_stderr);
    let reference = common::rendered_error_blocks(&result.reference_stderr, true);
    assert_eq!(reference.len(), 44, "{}", result.reference_stderr);
    assert_eq!(
        common::rendered_error_blocks(&result.krusty_stderr, false),
        reference
    );
}

/// Two different enabled modes are a configuration error that stops the compilation.
#[test]
fn conflicting_explicit_api_and_return_types_modes_are_rejected_like_kotlinc() {
    let result = common::compiler_diagnostics_with_shared_args(
        &[("main.kt", FIXTURE)],
        &[],
        &flags(&["-Xexplicit-api=strict", "-XXexplicit-return-types=warning"]),
    );
    let expected = "error: '-Xexplicit-api' and '-XXexplicit-return-types' flags cannot have \
                    different values at the same time.\nConsider use only one of those flags\n\
                    Passed:\n  '-Xexplicit-api=strict'\n  '-XXexplicit-return-types=warning'\n";
    assert_eq!(result.reference_code, 1);
    assert_eq!(result.reference_stderr, expected);
    assert_eq!(result.krusty_code, 1);
    assert_eq!(result.krusty_stderr, expected);
}
