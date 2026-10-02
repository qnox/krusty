//! A vararg's element type competes with a fixed parameter in one specificity
//! comparison. A strictly narrower element wins; equal types keep the declaration
//! that has no vararg. An empty call still prefers a defaulted parameter.
use super::common;

#[test]
fn vararg_element_specificity_follows_the_parameter_types() {
    let src = r#"
class All<T> {
    fun equalAny(vararg values: Any) = "vararg"
    fun equalAny(values: Any) = "fixed"

    fun equalT(vararg values: T) = "vararg"
    fun equalT(values: T) = "fixed"

    fun iterable(vararg values: T) = "vararg"
    fun iterable(values: Iterable<out T>) = "fixed"

    fun collection(vararg values: T) = "vararg"
    fun collection(values: Collection<T>) = "fixed"

    fun element(vararg values: Collection<T>) = "vararg"
    fun element(values: T) = "fixed"

    fun stringElement(vararg values: Collection<String>) = "vararg"
    fun stringElement(values: Any) = "fixed"

    fun stringCollection(vararg values: Collection<String>) = "vararg"
    fun stringCollection(values: Collection<T>) = "fixed"

    fun twoAny(vararg values: Collection<String>) = "vararg"
    fun twoAny(values: Any, values2: Any) = "fixed"

    fun twoT(vararg values: Collection<String>) = "vararg"
    fun twoT(values: T, values2: T) = "fixed"

    fun leading(values: Collection<String>, vararg values2: Collection<String>) = "vararg"
    fun leading(values: Any, values2: Any) = "fixed"

    fun defaults(x: String = "a") = "fixed"
    fun defaults(vararg x: String) = "vararg"
}

fun box(): String {
    val c: All<Any?> = All()
    val list: List<String> = listOf("")
    val parts = listOf(
        c.equalAny(list),
        c.equalT(list),
        c.iterable(list),
        c.collection(list),
        c.element(list),
        c.element(listOf("")),
        c.stringElement(list),
        c.stringCollection(list),
        c.twoAny(list, list),
        c.twoT(list, list),
        c.leading(list, list),
        c.defaults(),
        c.defaults("b"),
        c.equalAny(listOf("")),
        c.equalT(listOf("")),
        c.stringCollection(listOf("")),
        c.twoAny(listOf(""), listOf("")),
    )
    val expected = listOf(
        "fixed", "fixed", "fixed", "fixed",
        "vararg", "vararg", "vararg", "vararg",
        "vararg", "vararg", "vararg",
        "fixed", "fixed",
        "fixed", "fixed", "vararg", "vararg",
    )
    return if (parts == expected) "OK" else parts.toString()
}
"#;
    let (reference_code, reference_stderr) =
        common::kotlinc_source_result("VarargElementSpecificityReference", src);
    assert_eq!(
        reference_code, 0,
        "kotlinc rejected the vararg specificity fixture: {reference_stderr}"
    );
    assert_eq!(
        common::expect_box_run_with_stdlib(src, "VarargElementSpecificity"),
        "OK"
    );
}
