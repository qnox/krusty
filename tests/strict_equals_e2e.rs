//! Kotlin 2.5's strict equality: an `equals` whose parameter carries an `@EqualityBound`, declared
//! or inherited from the overridden `equals`, reads that parameter as the bound inside its body.
//! The call only reaches the body with such a value because the body opens with
//! `if (this === other) return true` (outside a value class) and `if (other !is Bound) return
//! false`. Each program must answer `OK` from krusty.
//!
//! The contract is the test-only `StrictEquals` language feature, which each program enables with a
//! `LANGUAGE` directive, so it runs on every supported release.

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

/// Every program answers `OK`. kotlinc 2.4 rejects `@EqualityBound` ("only available in Kotlin 2.5
/// and newer"), so the comparison with kotlinc runs on the 2.5 release.
fn assert_ok(stem: &str, source: &str) {
    assert_eq!(common::expect_box_run_with_stdlib(source, stem), "OK");
}

#[test]
fn an_equality_bound_equals_returns_before_its_body_for_itself_and_other_types() {
    assert_ok("IdentityAndTypeGuards", IDENTITY_AND_TYPE_GUARDS);
}

#[test]
fn an_inherited_equality_bound_guards_the_overriding_body() {
    assert_ok("InheritedBound", INHERITED_BOUND);
}

#[test]
fn a_value_class_equality_bound_guards_only_the_type() {
    assert_ok("ValueClassBound", VALUE_CLASS);
}

/// The bound is part of the overridden parameter's declaration, so it crosses a module boundary
/// in `@Metadata`: a consuming module's override without the annotation still reads `other` as
/// the bound. The library is compiled by krusty and consumed by kotlinc and by krusty.
const LIBRARY_BOUND: &str = "\
// LANGUAGE: +StrictEquals
open class Base(val n: Int) {
    override fun equals(@EqualityBound(Base::class) other: Any?): Boolean = n == other.n
}
";

const CONSUMER_OVERRIDE: &str = "\
// LANGUAGE: +StrictEquals
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

#[test]
fn a_library_equality_bound_reaches_a_consuming_override() {
    let library = common::compile_libs_build("LibraryBound", &[("Lib.kt", LIBRARY_BOUND)])
        .expect("krusty builds the library module");
    assert_eq!(
        common::kotlinc_box_result_with_classpath(
            CONSUMER_OVERRIDE,
            &[library.krusty_out().to_path_buf()],
        ),
        "OK",
        "kotlinc reads the bound from krusty's metadata",
    );
    assert_eq!(
        common::expect_box_run_against("LibraryBound", LIBRARY_BOUND, CONSUMER_OVERRIDE)
            .as_deref(),
        Some("OK"),
        "krusty reads the bound from its own metadata",
    );
}
