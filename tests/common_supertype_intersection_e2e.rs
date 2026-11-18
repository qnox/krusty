//! Sibling classes that share a class and an interface join to both, as kotlinc types them.

use super::common;

#[test]
fn a_join_of_siblings_keeps_every_most_specific_common_supertype() {
    const SOURCE: &str =
        include_str!("box_regressions/commonSupertypeOfSiblingsIsAnIntersection.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "common-supertype-intersection");
}

/// PLATFORM-SPECIFIC: the join also types a collection's element, and the stdlib collection bodies
/// (`listOf`, `map`) are not lowerable on the Native and Wasm KLIB lanes, so this part of the
/// program lives with the JVM e2e control instead of the shared regression.
#[test]
fn a_join_of_siblings_types_the_elements_of_a_collection() {
    common::expect_box_same_as_kotlinc(
        r#"
interface Named {
    val name: String
}

abstract class Expression<T>

abstract class TypedExpression<T> : Expression<T>()

class PlainAlias<T>(override val name: String) : Expression<T>(), Named

class TypedAlias<T>(override val name: String) : TypedExpression<T>(), Named

class Builder {
    operator fun Expression<*>.unaryPlus(): String = "expression"

    fun aliases(typed: Boolean): List<String> {
        val chosen = listOf(typed).map { if (it) TypedAlias<Int>("t") else PlainAlias<String>("p") }
        return chosen.map { it.name + ":" + (+it) }
    }
}

fun box(): String =
    if (Builder().aliases(true) == listOf("t:expression")) "OK" else "collection"
"#,
        "common-supertype-intersection-collection",
    );
}
