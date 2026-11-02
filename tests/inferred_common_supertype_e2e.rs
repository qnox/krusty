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

/// A contravariant argument of a joined classifier is the meet of the arguments: `Sink<A>` and
/// `Sink<AB>`, `AB : A`, join at `Sink<AB>`, which still accepts an `AB`. The inference join and
/// the branch join compute it with the same argument rules.
const CONTRAVARIANT_ARGUMENT_MEET: &str = r#"interface Sink<in T> { fun put(t: T): String }
interface A { fun a(): String = "O" }
interface B { fun b(): String = "K" }
class AB : A, B
class SinkA : Sink<A> { override fun put(t: A): String = t.a() }
class SinkAB : Sink<AB> { override fun put(t: AB): String = t.a() + t.b() }

fun <T> pick(first: T, second: T, takeFirst: Boolean): T = if (takeFirst) first else second

fun box(): String {
    val inferred = pick(SinkA(), SinkAB(), takeFirst = false)
    val branch = if (inferred.put(AB()) == "OK") SinkA() else SinkAB()
    val text = inferred.put(AB()) + branch.put(AB())
    return if (text == "OKO") "OK" else text
}
"#;

#[test]
fn a_contravariant_argument_joins_at_the_meet_of_the_arguments() {
    common::expect_box_same_as_kotlinc(CONTRAVARIANT_ARGUMENT_MEET, "InferredContravariantMeet");
}

/// Unrelated contravariant arguments meet at their intersection, as kotlinc's common supertype
/// intersects them: `Sink<A>` and `Sink<B>` join at `Sink<A & B>`, which accepts an `AB` that is
/// both. An empty meet at `Nothing` would reject the call.
const CONTRAVARIANT_ARGUMENT_INTERSECTION: &str = r#"interface Sink<in T> { fun put(t: T): String }
interface A { fun a(): String = "O" }
interface B { fun b(): String = "K" }
class AB : A, B
class SinkA : Sink<A> { override fun put(t: A): String = t.a() }
class SinkB : Sink<B> { override fun put(t: B): String = t.b() }

fun <T> pick(first: T, second: T, takeFirst: Boolean): T = if (takeFirst) first else second

fun box(): String {
    val first = pick(SinkA(), SinkB(), takeFirst = true)
    val second = pick(SinkA(), SinkB(), takeFirst = false)
    return first.put(AB()) + second.put(AB())
}
"#;

#[test]
fn unrelated_contravariant_arguments_meet_at_their_intersection() {
    common::expect_box_same_as_kotlinc(
        CONTRAVARIANT_ARGUMENT_INTERSECTION,
        "InferredContravariantIntersection",
    );
}

/// A reified argument cannot carry an intersection, so it is approximated by the position the
/// intersection occupies: a contravariant one reifies as `Nothing`, a covariant one as the common
/// supertype, and an invariant one as an `out` projection of it. kotlinc must agree on every
/// rendered type for the fixture to return `OK`.
const REIFIED_INTERSECTION_ARGUMENTS: &str = r#"import kotlin.reflect.typeOf
class In<in T>
class Out<out T>
class Inv<T>
interface A
interface B

@OptIn(kotlin.ExperimentalStdlibApi::class)
inline fun <reified K> reify(first: K, second: K): String =
    typeOf<K>().toString().substringBefore(" (") + " " +
        Array<K>(1) { first }.javaClass.componentType.simpleName

fun box(): String {
    val rendered = listOf(
        reify(In<A>(), In<B>()),
        reify(Out<A>(), Out<B>()),
        reify(Inv<A>(), Inv<B>()),
    )
    val expected = listOf(
        "In<kotlin.Nothing> In",
        "Out<java.lang.Object> Out",
        "Inv<out java.lang.Object> Inv",
    )
    return if (rendered == expected) "OK" else rendered.toString()
}
"#;

#[test]
fn a_reified_intersection_argument_is_approximated_by_its_position() {
    common::expect_box_same_as_kotlinc(
        REIFIED_INTERSECTION_ARGUMENTS,
        "ReifiedIntersectionArguments",
    );
}
