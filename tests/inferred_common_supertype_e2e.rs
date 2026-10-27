//! A type variable inferred from arguments of different classes is their most specific shared
//! supertype, however far up each hierarchy it sits: `listOf(Leaf(), Group())` with `Leaf : Node`
//! and `Group : Container`, `Container : Node`, is a `List<Node>`, not a `List<Any>`.
//!
//! From quarkdown's `ReferenceDefinitionFootnote.children`, whose two elements share `Node` through
//! interfaces at different depths.

use super::common;

const SHARED_INTERFACE: &str = r#"interface Node { fun render(): String }
interface Container : Node
interface Labelled : Container
interface Tagged
class Leaf(private val text: String) : Node { override fun render() = text }
class Group : Labelled, Tagged { override fun render() = "[group]" }
class Reference<R, D>(private val label: String) : Node { override fun render() = "<$label>" }

fun <T> pair(first: T, second: T): List<T> = listOf(first, second)
fun renderAll(nodes: List<Node>): String = nodes.joinToString("") { it.render() }

val children = listOf(Leaf("a"), Group())
fun reversed() = pair(Group(), Reference<Leaf, Group>("b"))

fun box(): String {
    val local = listOf(Reference<Group, Leaf>("c"), Leaf("d"), Group())
    val text = renderAll(children) + renderAll(reversed()) + renderAll(local)
    return if (text == "a[group][group]<b><c>d[group]") "OK" else text
}
"#;

#[test]
fn inferred_type_variable_joins_at_the_most_specific_shared_supertype() {
    common::expect_box_same_as_kotlinc(SHARED_INTERFACE, "InferredCommonSupertype");
}
