//! A local delegated property declared in a literal lambda that an inline call inlines: the
//! inlined body is emitted code, so its delegate accesses call the property's accessor and its
//! `KProperty` operand reads the class's `$$delegatedProperties`, as in the lambda's own body. The
//! inline function is this repository's own, compiled by the reference compiler.

use super::common;

const LIB: &str = r#"
inline fun <T, R> withIt(value: T, block: (T) -> R): R = block(value)
"#;

const MAIN: &str = r#"
import kotlin.reflect.KProperty

class Holder(val value: String)

operator fun Holder.provideDelegate(thiz: Any?, property: KProperty<*>): Holder = this

operator fun Holder.getValue(thiz: Any?, property: KProperty<*>): String = value

fun box(): String = withIt(Holder("OK")) {
    val result by it
    result
}
"#;

#[test]
fn a_delegated_local_in_an_inlined_lambda_runs() {
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, MAIN)
            .expect("reference kotlinc is provisioned"),
        "OK"
    );
}

#[test]
fn a_delegated_local_in_an_inlined_lambda_is_compiled_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_lib("Main", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
