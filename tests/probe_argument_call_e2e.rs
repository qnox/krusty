//! A nested call typed before the outer call has selected a candidate is a probe.
//!
//! `instance(mapOf("m" to emptyList()))` on a member (or an overloaded) `instance` types its
//! argument before any candidate supplies `Map<String, List<Cfg>>`. Without that expectation the
//! probe cannot infer `mapOf`'s variables; the selected candidate re-checks the argument against
//! its parameter, and only that verdict is reported.

use super::common;
use super::diagnostics_parity_support::{errors, ObservedError};

#[test]
fn a_nested_generic_argument_takes_the_selected_parameter() {
    common::expect_box_same_as_kotlinc(
        r#"
class Cfg(val port: Int)

class Registry {
    fun instance(endpoints: Map<String, List<Cfg>>): Int = endpoints.size
    fun register(): Int = instance(mapOf("m" to emptyList()))
}

fun overloaded(endpoints: Map<String, List<Cfg>>): Int = endpoints.size
fun overloaded(endpoints: Map<String, List<Cfg>>, extra: Int): Int = endpoints.size + extra

fun box(): String {
    if (Registry().register() != 1) return "member"
    if (overloaded(mapOf("a" to emptyList(), "b" to listOf(Cfg(1)))) != 2) return "overloaded"
    return "OK"
}
"#,
        "ProbeArgumentCall",
    );
}

#[test]
fn the_selected_parameter_still_reports_a_real_mismatch() {
    let source = "class Cfg(val port: Int)\n\
                  \n\
                  class Registry {\n    \
                      fun instance(endpoints: Map<String, List<Cfg>>): Int = 1\n    \
                      fun ints() { instance(mapOf(\"m\" to listOf(1))) }\n    \
                      fun unresolved() { instance(mapOf(\"m\" to listOf(undefinedThing))) }\n\
                  }\n";
    let result = common::compiler_diagnostics(&[("Use.kt", source)], &[common::stdlib_jar()]);
    assert_eq!((result.krusty_code, result.reference_code), (1, 1));
    let error = |line, column, message: &str| ObservedError {
        file: "Use.kt".to_string(),
        line,
        column,
        message: message.to_string(),
    };
    let expected = vec![
        error(
            5,
            27,
            "argument type mismatch: actual type is 'Map<String, List<Int>>', but 'Map<String, List<Cfg>>' was expected.",
        ),
        error(6, 53, "unresolved reference 'undefinedThing'."),
    ];
    let mut krusty = errors(&result.krusty_stderr);
    krusty.extend(errors(&result.krusty_stdout));
    assert_eq!(krusty, expected);
    assert_eq!(errors(&result.reference_stderr), expected);
}
