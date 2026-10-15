//! Java members used with Kotlin function values preserve an enclosing declaration's type-parameter
//! identity. That identity is fixed inside the enclosing body; it is not one of the selected
//! callable's still-open formals.

use super::common;

const SAM_RESULT: &str = r#"import java.util.ArrayList
import java.util.HashMap

fun <T> addValue(m: HashMap<String, MutableList<T>>, key: String, value: T) {
    m.computeIfAbsent(key) { ArrayList() }.add(value)
}

fun box(): String {
    val groups = HashMap<String, MutableList<Int>>()
    addValue(groups, "numbers", 1)
    addValue(groups, "numbers", 2)
    return if (groups["numbers"] == listOf(1, 2)) "OK" else "fail: $groups"
}
"#;

const CALLABLE_REFERENCE_RESULT: &str = r#"import java.util.function.Supplier

fun <X> id(value: () -> X): X = value()

fun <R> supplied(provider: Supplier<R>): R = id(provider::get)

fun box(): String = supplied(Supplier { "OK" })
"#;

#[test]
fn a_java_member_sam_result_keeps_the_enclosing_type_parameter() {
    common::expect_box_same_as_kotlinc(SAM_RESULT, "JavaMemberSamOuterTypeParameter");
}

#[test]
fn a_dependency_callable_reference_keeps_its_result_type_parameter() {
    common::expect_box_same_as_kotlinc(
        CALLABLE_REFERENCE_RESULT,
        "DependencyCallableReferenceTypeParameter",
    );
}
