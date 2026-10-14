//! A call that omits arguments of a dependency's `@InlineOnly` function inlines the function's
//! `$default` bridge as kotlinc's `IrInlineCodegen` does: the constant masks resolve the bridge's
//! mask header, the masks and the method handle are no parameters of the inlined body, and an
//! omitted parameter only reserves its temporary. A `Unit` lambda ends on its closing-brace `nop`
//! before the inliner loads `Unit.INSTANCE`, and a discarded generic result is popped as the
//! erased value the call returns.

use super::common;

const LIB: &str = r#"
@file:Suppress("INVISIBLE_REFERENCE", "INVISIBLE_MEMBER")
package lib

var seen: Any? = null

fun use(value: Any?) {
    seen = value
}

@kotlin.internal.InlineOnly
inline fun defaulted(message: String? = null, count: Int = 3, block: () -> Unit): Int {
    val copy = message
    block()
    use(copy)
    return count
}

@kotlin.internal.InlineOnly
inline fun <reified T : Any> named(prefix: String = "<", block: () -> T): T {
    val value = block()
    use(prefix + T::class.java.simpleName)
    return value
}

@kotlin.internal.InlineOnly
inline fun wide(value: Long = 7L, block: () -> Unit): Long {
    block()
    return value
}
"#;

const MAIN: &str = r#"
import lib.*

fun omitted(seen: Int): Int = defaulted { use(seen) }

fun middle(): Int = defaulted(count = 4) {
    use(null)
}

fun supplied(): Int = defaulted("s", 2) { }

fun discarded() {
    named { "x" }
}

fun wideDefault(): Long = wide { use("wide") }
"#;

#[test]
fn an_omitted_argument_of_an_inline_only_function_is_inlined_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_lib("Main", &[("Lib.kt", LIB)], MAIN)
        .expect("reference kotlinc is provisioned");
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn a_discarded_generic_dependency_result_is_popped_like_the_reference_compiler() {
    let classes = common::classes_against_kotlinc_module(&[(
        "First.kt",
        r#"
fun firsts(values: List<String>) {
    values.first()
    values.firstOrNull { it.length > 1 }
}
"#,
    )]);
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
