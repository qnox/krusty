//! A class whose superclass is a Kotlin collection base (`kotlin.collections.AbstractCollection`,
//! `AbstractList`, `AbstractSet`, `AbstractMap`) inherits that base's generic members as fake
//! overrides in its own type arguments, so like kotlinc it writes the bridges a Java collection
//! superclass would get: `contains(String)` calling the base's `contains(Object)` with
//! `invokespecial`, then the final type-safe `contains(Object)` that tests its argument. The fake
//! overrides come from the class's own read-only faces and are mapped like Kotlin declarations: a
//! substituted `Int` parameter is `int` (`containsValue(int)` boxes it for the base), the result of
//! a type parameter is boxed (`Integer get(String)`), and their locals keep the builtin's names.
//! The inherited `java.util.Map.getOrDefault` default joins them. Each class is compared byte for
//! byte with kotlinc's, and Java's `java.util` interface calls reach the members.

use super::common;

const ABSTRACT_BASES: &str = "abstract class Items : AbstractCollection<String>()
abstract class Rows : AbstractList<String>()
abstract class Tags : AbstractSet<String>()
abstract class Counts : AbstractMap<String, Int>()
";

#[test]
fn a_kotlin_collection_base_leaves_its_type_safe_bridges_to_the_class_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "KotlinCollectionBases",
        ABSTRACT_BASES,
        &["Items", "Rows", "Tags", "Counts"],
    );
}

const CONCRETE_BASES: &str = "class Names : AbstractList<String>() {
    override val size: Int get() = 2
    override fun get(index: Int): String = \"n$index\"
}

class Totals : AbstractMap<String, Int>() {
    override val entries: Set<Map.Entry<String, Int>> get() = mapOf(\"a\" to 1).entries
}

class Labels : AbstractSet<String>() {
    override val size: Int get() = 1
    override fun iterator(): Iterator<String> = listOf(\"t\").iterator()
}
";

#[test]
fn a_concrete_kotlin_collection_subclass_gets_the_bridges_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "KotlinCollectionSubclasses",
        CONCRETE_BASES,
        &["Names", "Totals", "Labels"],
    );
}

#[test]
fn java_reaches_a_kotlin_collection_subclass_through_its_bridges_like_kotlinc() {
    let program = format!(
        "{CONCRETE_BASES}
fun call(owner: Class<*>, receiver: Any, name: String, vararg arguments: Any?): Any? =
    owner.methods.single {{ it.name == name && it.parameterCount == arguments.size }}
        .invoke(receiver, *arguments)

fun box(): String {{
    val list = Names()
    val map = Totals()
    val set = Labels()
    val outcome = listOf(
        call(java.util.Collection::class.java, list, \"contains\", \"n1\"),
        call(java.util.Collection::class.java, list, \"contains\", 1),
        call(java.util.List::class.java, list, \"indexOf\", \"n1\"),
        call(java.util.List::class.java, list, \"lastIndexOf\", 1),
        call(java.util.Map::class.java, map, \"containsValue\", 1),
        call(java.util.Map::class.java, map, \"containsValue\", \"1\"),
        call(java.util.Map::class.java, map, \"getOrDefault\", \"a\", 5),
        call(java.util.Map::class.java, map, \"getOrDefault\", \"b\", 5),
        call(java.util.Map::class.java, map, \"getOrDefault\", 3, 5),
        call(java.util.Map::class.java, map, \"get\", 3),
        call(java.util.Collection::class.java, set, \"contains\", \"t\"),
        call(java.util.Collection::class.java, set, \"contains\", 2),
    ).joinToString()
    val expected = \"true, false, 1, -1, true, false, 1, 5, 5, null, true, false\"
    return if (outcome == expected) \"OK\" else outcome
}}
"
    );
    common::expect_box_same_as_kotlinc(&program, "kotlin_collection_superclass_bridges");
}
