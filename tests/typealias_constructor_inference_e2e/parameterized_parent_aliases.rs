//! The complete expanded constructor-alias regression retained from #2655.

// PLATFORM: complete JVM class ABI and reference/runtime comparison.
use super::{assert_complete_class_abi, common};

const ANONYMOUS_PARAMETERIZED_PARENT_ALIASES: &str = r#"
open class Carrier<T>(val value: T)
interface Reader<out T> { fun read(): T }
typealias Nested<T> = Carrier<List<T>>
typealias NestedStrings = Nested<String>
typealias Reads<T> = Reader<List<T>>
val fixed = object : NestedStrings(listOf("OK")) {}
private fun <T> copy(value: T) = object : Nested<T>(listOf(value)) {}
private fun <T> reader(value: T) = object : Reads<T> {
    override fun read() = listOf(value)
}
fun copied() = copy("OK")
fun readable() = reader("OK")
fun box(): String = if (fixed.value.single() == "OK" &&
    copied().value.single() == "OK" && readable().read().single() == "OK") "OK" else "value"
"#;

#[test]
fn anonymous_factory_parameterized_parent_alias_class_abi_matches_kotlinc() {
    assert_complete_class_abi(&common::classes_against_kotlinc_module(&[(
        "AnonymousParameterizedParentAliases.kt",
        ANONYMOUS_PARAMETERIZED_PARENT_ALIASES,
    )]));
}
#[test]
fn anonymous_factory_parameterized_parent_alias_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(
        ANONYMOUS_PARAMETERIZED_PARENT_ALIASES,
        "anonymous_parameterized_parent_aliases",
    );
}
