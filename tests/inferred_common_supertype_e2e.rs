//! A type variable inferred from arguments of different classes is their common supertype: every
//! most-specific supertype both share, however far up each hierarchy it sits, and their
//! intersection when several incomparable ones remain.
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

/// Incomparable most-specific shared supertypes join at their intersection: `A : Base(), I` and
/// `B : Base(), J` with `J : I` share both `Base` and `I`, and the inferred type keeps the members
/// of each.
const INCOMPARABLE_SHARED_SUPERTYPES: &str = r#"open class Base { fun baseMember(): String = "base" }
interface I { fun iMember(): String = "i" }
interface J : I
class A : Base(), I
class B : Base(), J

fun <T> pick(a: T, b: T): T = a

fun box(): String {
    val picked = pick(A(), B())
    val both = listOf(A(), B())
    val text = picked.baseMember() + picked.iMember() + both.first().baseMember() + both.last().iMember()
    return if (text == "baseibasei") "OK" else text
}
"#;

#[test]
fn incomparable_shared_supertypes_join_at_their_intersection() {
    common::expect_box_same_as_kotlinc(
        INCOMPARABLE_SHARED_SUPERTYPES,
        "InferredIncomparableSupertypes",
    );
}
