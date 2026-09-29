//! A receiver-less classifier callable reference (`Classifier::member`) is the same family as
//! `Classifier.member(...)`. An expected function type selects among those static overloads while
//! an inferred signature is still being solved, including a Java class that has no companion.

use super::common::expect_box_same_as_kotlinc;

#[test]
fn inferred_java_static_callable_reference_selects_by_expected_function() {
    expect_box_same_as_kotlinc(
        r#"
fun versions(keys: List<String>) = keys.map(System::getProperty)
fun parsed(texts: List<String>) = texts.map(Integer::parseInt)
fun lengths(values: List<String>) = values.map(String::length)
val names = listOf("java.version")
val home get() = names.firstNotNullOfOrNull(System::getProperty)
fun box(): String {
    val found = versions(names)
    if (found.size != 1 || found[0] == null || found[0]!!.isEmpty()) return "map"
    if (parsed(listOf("2", "10")) != listOf(2, 10)) return "parse"
    if (lengths(listOf("ab", "c")) != listOf(2, 1)) return "length"
    if (home == null || home!!.isEmpty()) return "home"
    return "OK"
}
"#,
        "JavaStaticCallableReference",
    );
}
