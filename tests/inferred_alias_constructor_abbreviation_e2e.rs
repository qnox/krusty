//! A constructor call through a type alias abbreviates its published type with the alias applied
//! to the same arguments. An argument inferred through a Java usage is flexible in both.

use super::common;

const INFERRED: &str = r#"
val copied = ArrayList(listOf("a"))
val set = LinkedHashSet(setOf(1))
val map = HashMap(mapOf("a" to 1))
val nested = ArrayList(listOf(listOf("a")))

class Holder(names: List<String>) {
    val members = ArrayList(names)
}
"#;

#[test]
fn inferred_alias_constructor_arguments_abbreviate_the_published_type() {
    common::assert_classes_identical_to_kotlinc(
        "InferredAliasConstructor",
        INFERRED,
        &["InferredAliasConstructorKt", "Holder"],
    );
}
