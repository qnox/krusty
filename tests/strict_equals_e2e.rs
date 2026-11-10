//! Kotlin 2.5's strict equality: an `equals` whose parameter carries an `@EqualityBound`, declared
//! or inherited from the overridden `equals`, reads that parameter as the bound inside its body.
//! The call only reaches the body with such a value because the body opens with
//! `if (this === other) return true` (outside a value class) and `if (other !is Bound) return
//! false`. Each program must answer `OK` from kotlinc and from krusty.
//!
//! `EqualityBound` is a Kotlin 2.5 declaration: an older reference release rejects every program
//! here because its API version does not have it.

use krusty::kotlin_version::KotlinVersion;

use super::common;

/// `===` returns before the body, and only a value of the bound reaches it.
const IDENTITY_AND_TYPE_GUARDS: &str = "\
// LANGUAGE: +StrictEquals
var calls = 0

open class Base(val n: Int) {
    override fun equals(@EqualityBound(Base::class) other: Any?): Boolean {
        calls++
        return n == other.n
    }
}

class Child(n: Int, val extra: String) : Base(n) {
    override fun equals(@EqualityBound(Child::class) other: Any?): Boolean =
        n == other.n && extra == other.extra
}

fun box(): String {
    val base = Base(1)
    if (base != base) return \"identity\"
    if (calls != 0) return \"identity reached the body: $calls\"
    if (base != Base(1)) return \"equal\"
    if (calls != 1) return \"body calls: $calls\"
    if (Child(1, \"a\") == Base(1)) return \"a Base reached Child.equals\"
    if (Base(1).equals(\"1\")) return \"a String reached Base.equals\"
    if (Base(1).equals(null)) return \"null reached Base.equals\"
    return \"OK\"
}
";

/// An override without the annotation inherits the bound, and with it the guards.
const INHERITED_BOUND: &str = "\
// LANGUAGE: +StrictEquals
open class Base(val n: Int) {
    override fun equals(@EqualityBound(Base::class) other: Any?): Boolean = n == other.n
}

class Overriding(n: Int) : Base(n) {
    override fun equals(other: Any?): Boolean = n == other.n
}

fun box(): String {
    if (Overriding(1) != Base(1)) return \"equal\"
    if (Overriding(1) == Overriding(2)) return \"different\"
    if (Overriding(1) == Any()) return \"an Any reached Overriding.equals\"
    return \"OK\"
}
";

/// A value class has no identity: its `equals` keeps only the type test.
const VALUE_CLASS: &str = "\
// LANGUAGE: +StrictEquals +CustomEqualsInValueClasses
var calls = 0

@JvmInline
value class Id(val raw: Int) {
    override fun equals(@EqualityBound(Id::class) other: Any?): Boolean {
        calls++
        return raw == other.raw
    }
}

fun box(): String {
    val id: Any = Id(1)
    if (id != id) return \"equal\"
    if (calls == 0) return \"an identity guard skipped the value-class body\"
    if (id == 1) return \"an Int reached Id.equals\"
    return \"OK\"
}
";

fn assert_ok_like_kotlinc(stem: &str, source: &str) {
    if krusty::kotlin_version::target() < KotlinVersion::V2_5_0_BETA2 {
        return;
    }
    common::expect_box_same_as_kotlinc(source, stem);
}

#[test]
fn an_equality_bound_equals_returns_before_its_body_for_itself_and_other_types() {
    assert_ok_like_kotlinc("IdentityAndTypeGuards", IDENTITY_AND_TYPE_GUARDS);
}

#[test]
fn an_inherited_equality_bound_guards_the_overriding_body() {
    assert_ok_like_kotlinc("InheritedBound", INHERITED_BOUND);
}

#[test]
fn a_value_class_equality_bound_guards_only_the_type() {
    assert_ok_like_kotlinc("ValueClassBound", VALUE_CLASS);
}
