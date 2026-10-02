//! A type variable with only incompatible upper bounds is the intersection of those
//! bounds. The call still returns a value; it is not a divergent `Nothing`.

use super::common;

fn expect_method_matches(name: &str, src: &str, class: &str, method: &str) {
    match common::method_code_diff_against_kotlinc(name, &[], src, class, method) {
        None => panic!("reference kotlinc is provisioned"),
        Some(Ok(())) => {}
        Some(Err(difference)) => panic!("{difference}"),
    }
}

const INTERSECT: &str = "\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n\
fun witness(n: Int, s: String) { }\n\
fun check() {\n\
    val a = intersect(In<Int>(), In<String>())\n\
    witness(a, a)\n\
}\n\
fun box(): String {\n\
    val a = intersect(In<Int>(), In<String>())\n\
    val b = intersect(In<Int>(), *arrayOf(In<String>()))\n\
    val c = intersect(x = arrayOf(In<Int>(), In<String>()))\n\
    val d = intersect(x = *arrayOf(In<Int>(), In<String>()))\n\
    val e = intersect(In<Int>(), *arrayOf(In<String>()), In<Long>())\n\
    val f = intersect(*arrayOf(In<Int>()), In<String>(), *arrayOf(In<Long>()))\n\
    return if (a == null && a == b && b == c && c == d && d == e && e == f) \"OK\" else \"NOT_OK\"\n\
}\n";

#[test]
fn incompatible_upper_bounds_return_a_null_intersection() {
    common::expect_box_same_as_kotlinc(INTERSECT, "EmptyIntersection");
}

#[test]
fn an_intersection_is_not_nothing_without_an_expected_nothing() {
    const SRC: &str = "\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n\
fun take(x: Nothing): Any = x\n\
fun box(): String {\n\
    val a = intersect(In<Int>(), In<String>())\n\
    take(a)\n\
    return \"OK\"\n\
}\n";
    common::assert_errors_match_kotlinc(&[("EmptyIntersectionNothing.kt", SRC)], &[]);
}

#[test]
fn one_contravariant_upper_bound_is_that_bound() {
    const SRC: &str = "\
class Context<T>\n\
fun <T> select(value: Context<in T>): T = null as T\n\
fun box(): String {\n\
    val a = select(Context<Any>())\n\
    return if (a == null) \"OK\" else \"NO\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "SingleUpperBound");
}

#[test]
fn an_expected_nothing_result_still_instantiates_nothing() {
    const SRC: &str = "\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n\
fun probe(): Nothing = intersect(In<Int>(), In<String>())\n\
fun box(): String = \"OK\"\n";
    common::expect_box_same_as_kotlinc(SRC, "ExpectedNothingIntersection");
}

const SHAPES: &str = "\
interface Left { fun left(): String; val n: Int }\n\
interface Right { fun right(): String; val n: Int }\n\
abstract class Base\n\
class Both : Base(), Left, Right {\n\
    override fun left() = \"L\"\n\
    override fun right() = \"R\"\n\
    override val n = 1\n\
}\n\
class In<in K>\n\
class Inv<T>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n";

#[test]
fn nullable_bounds_admit_null_and_a_mixed_bound_does_not() {
    const SRC: &str = "\
interface Left\n\
interface Right\n\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n\
fun take(x: Any) {}\n\
fun box(): String {\n\
    val nullable = intersect(In<Left?>(), In<Right?>())\n\
    take(nullable)\n\
    return \"OK\"\n\
}\n";
    common::assert_errors_match_kotlinc(&[("NullableIntersection.kt", SRC)], &[]);
}

#[test]
fn a_mixed_nullable_intersection_is_not_nullable() {
    const SRC: &str = "\
interface Left { fun left(): String }\n\
interface Right\n\
class Both : Left, Right {\n\
    override fun left() = \"L\"\n\
}\n\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = Both() as E\n\
fun take(x: Any) {}\n\
fun box(): String {\n\
    val mixed = intersect(In<Left?>(), In<Right>())\n\
    take(mixed)\n\
    return if (mixed.left() == \"L\") \"OK\" else \"NO\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "MixedIntersection");
}

#[test]
fn a_nullable_bottom_beside_a_non_null_bound_is_nothing() {
    const SRC: &str = "\
interface Left\n\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n\
fun nonNullBottom() = intersect(In<Nothing?>(), In<Left>())\n\
fun box(): String = \"OK\"\n";
    common::assert_errors_match_kotlinc(&[("NullBottomIntersection.kt", SRC)], &[]);
}

#[test]
fn a_nullable_bottom_beside_a_nullable_bound_is_not_nothing() {
    const SRC: &str = "\
interface Left\n\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n\
fun take(x: Any) {}\n\
fun box(): String {\n\
    val nullable = intersect(In<Nothing?>(), In<Left?>())\n\
    take(nullable)\n\
    return \"OK\"\n\
}\n";
    common::assert_errors_match_kotlinc(&[("NullableBottomIntersection.kt", SRC)], &[]);
}

#[test]
fn an_intersection_exposes_members_properties_and_references() {
    const SRC: &str = "\
interface Left { fun left(): String; val n: Int }\n\
interface Right { fun right(): String; val n: Int }\n\
class Both : Left, Right {\n\
    override fun left() = \"L\"\n\
    override fun right() = \"R\"\n\
    override val n = 1\n\
}\n\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = Both() as E\n\
fun box(): String {\n\
    val x = intersect(In<Left>(), In<Right>())\n\
    val f = x::left\n\
    return if (x.left() == \"L\" && x.right() == \"R\" && x.n == 1 && f() == \"L\") \"OK\" else \"NO\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "IntersectionMembers");
}

#[test]
fn an_intersection_call_uses_the_canonical_components_default() {
    // Both components declare `choose` with a default. Kotlin 2.4.20 accepts the call and uses
    // the canonical-earliest component, which is also the encounter order of `Left` then `Right`.
    const SRC: &str = "\
interface Left { fun choose(value: Int = 1): String = \"L$value\" }\n\
interface Right { fun choose(value: Int = 2): String = \"R$value\" }\n\
class In<in K>\n\
class OnlyLeft : Left\n\
fun <E> intersect(vararg x: In<E>): E = OnlyLeft() as E\n\
fun box(): String {\n\
    val value = intersect(In<Left>(), In<Right>())\n\
    return if (value.choose() == \"L1\") \"OK\" else \"NO\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "IntersectionComponentDefault");
}

#[test]
fn a_canonical_component_can_inherit_the_selected_default() {
    // `Left` does not declare `choose`. Its nearest inherited slot still precedes `Right`'s
    // direct declaration, so the call takes Shared's default. The witness implements both
    // interfaces, which is what the intersection cast requires.
    const SRC: &str = "\
interface Shared { fun choose(value: Int = 1): String = \"B$value\" }\n\
interface Left : Shared\n\
interface Right { fun choose(value: Int = 2): String = \"R$value\" }\n\
class In<in K>\n\
class Both : Left, Right {\n\
    override fun choose(value: Int): String = \"B$value\"\n\
}\n\
fun <E> intersect(vararg x: In<E>): E = Both() as E\n\
fun box(): String {\n\
    val value = intersect(In<Left>(), In<Right>())\n\
    return if (value.choose() == \"B1\") \"OK\" else \"NO\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "InheritedIntersectionComponentDefault");
}

#[test]
fn a_public_intersection_matches_kotlinc_abi_and_metadata() {
    let source = format!(
        "{SHAPES}\n\
fun published() = intersect(In<Left>(), In<Right>())\n\
fun classBound() = intersect(In<Base>(), In<Left>())\n\
fun nullableBounds() = intersect(In<Left?>(), In<Right?>())\n\
fun sameClass() = intersect(In<Inv<String>>(), In<Inv<Int>>())\n\
fun lists() = intersect(In<List<String>>(), In<List<Int>>())\n\
val prop get() = intersect(In<Left>(), In<Right>())\n"
    );
    let comparison = common::compare_with_kotlinc_plugin(
        "IntersectionAbi",
        &source,
        "IntersectionAbiKt",
        &[common::stdlib_jar()],
        "17",
        &[],
    )
    .expect("reference kotlinc and javap are provisioned");
    assert_eq!(
        common::member_table(&comparison.krusty_bytes),
        common::member_table(&comparison.reference_bytes),
        "intersection declaration descriptors"
    );
    assert_eq!(
        common::raw_kotlin_metadata(&comparison.krusty_bytes),
        common::raw_kotlin_metadata(&comparison.reference_bytes),
        "intersection declaration metadata"
    );
}

#[test]
fn an_override_may_infer_nothing_from_its_expression_body() {
    const SRC: &str = "\
interface Flags { fun enabled(): Boolean }\n\
class Off : Flags {\n\
    override fun enabled() = throw RuntimeException(\"off\")\n\
}\n\
fun box(): String =\n\
    try { Off().enabled(); \"called\" }\n\
    catch (e: RuntimeException) { if (e.message == \"off\") \"OK\" else \"msg\" }\n";
    common::expect_box_same_as_kotlinc(SRC, "OverrideNothingBody");
}

#[test]
fn an_inherited_property_precedes_a_later_components_declaration() {
    // `Left` does not declare `mark`. The property it inherits from `Shared` still precedes
    // `Right`'s direct declaration, so both the read and the callable reference bind to
    // `Shared.getMark`. `Both` overrides the property, so the runtime value does not reveal
    // the owner.
    const SRC: &str = "\
interface Shared { val mark: Int get() = 1 }\n\
interface Left : Shared\n\
interface Right { val mark: Int get() = 2 }\n\
class In<in K>\n\
class Both : Left, Right {\n\
    override val mark: Int get() = 0\n\
}\n\
fun <E> intersect(vararg x: In<E>): E = Both() as E\n\
fun read(): Int {\n\
    val value = intersect(In<Left>(), In<Right>())\n\
    return value.mark\n\
}\n\
fun ref(): Int {\n\
    val value = intersect(In<Left>(), In<Right>())\n\
    return value::mark.get()\n\
}\n\
fun box(): String = if (read() == 0 && ref() == 0) \"OK\" else \"NO\"\n";
    expect_method_matches(
        "InheritedIntersectionProperty",
        SRC,
        "InheritedIntersectionPropertyKt",
        "public static final int read(",
    );
    expect_method_matches(
        "InheritedIntersectionProperty",
        SRC,
        "InheritedIntersectionPropertyKt$ref$1",
        "public java.lang.Object get(",
    );
}

#[test]
fn a_later_var_overrides_an_earlier_inherited_val() {
    const SRC: &str = "\
interface Shared { val mark: Int get() = 1 }\n\
interface Left : Shared\n\
interface Right {\n\
    var mark: Int\n\
        get() = 2\n\
        set(value) { calls += value }\n\
}\n\
var calls = 0\n\
class In<in K>\n\
class Both : Left, Right {\n\
    override var mark: Int\n\
        get() = 2\n\
        set(value) { calls += value }\n\
}\n\
fun <E> intersect(vararg x: In<E>): E = Both() as E\n\
fun box(): String {\n\
    val value = intersect(In<Left>(), In<Right>())\n\
    value.mark = 3\n\
    val ref = value::mark\n\
    ref.set(4)\n\
    return if (value.mark == 2 && calls == 7) \"OK\" else \"NO\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "IntersectionVarOverridesVal");
}

#[test]
fn an_intersection_of_vals_cannot_be_reassigned() {
    const SRC: &str = "\
interface Shared { val mark: Int get() = 1 }\n\
interface Left : Shared\n\
interface Right { val mark: Int get() = 2 }\n\
class In<in K>\n\
fun <E> intersect(vararg x: In<E>): E = null as E\n\
fun box() {\n\
    val value = intersect(In<Left>(), In<Right>())\n\
    value.mark = 1\n\
}\n";
    common::assert_errors_match_kotlinc(&[("IntersectionValReassignment.kt", SRC)], &[]);
}

#[test]
fn a_class_keeps_both_supertype_properties_for_the_accessor_bridge() {
    const SRC: &str = "\
open class A<T> { var size: T = 56 as T }\n\
interface C { var size: Int }\n\
class B : C, A<Int>()\n\
fun box(): String {\n\
    val b = B()\n\
    if (b.size != 56) return \"init\"\n\
    b.size = 55\n\
    val c: C = b\n\
    if (c.size != 55) return \"iface\"\n\
    c.size = 57\n\
    return if (b.size == 57) \"OK\" else \"write\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "SupertypePropertyBridge");
}
