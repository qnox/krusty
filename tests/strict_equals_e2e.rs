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
/// the bound. The library is compiled by krusty and consumed by krusty; kotlinc 2.4 rejects `@EqualityBound`, so
/// the kotlinc consumer runs on the 2.5 release.
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
    // The library is built by krusty and consumed by krusty alone: the default library
    // cross-check would compile it with kotlinc 2.4, which rejects `@EqualityBound`.
    let library = common::compile_libs_build("LibraryBound", &[("Lib.kt", LIBRARY_BOUND)])
        .expect("krusty builds the library module");
    let jdk = common::jdk_modules();
    assert_eq!(
        common::compile_and_run_box(
            CONSUMER_OVERRIDE,
            "Consumer",
            &[library.krusty_out().to_path_buf(), common::stdlib_jar()],
            Some(jdk.as_path()),
        )
        .as_deref(),
        Some("OK"),
        "krusty reads the bound from its own metadata",
    );
}

/// kotlinc 2.4 reports `@EqualityBound` as a declaration of Kotlin 2.5 and newer.
fn kotlinc_admits_equality_bound() -> bool {
    use krusty::kotlin_version::KotlinVersion;
    common::for_reference(&[
        (KotlinVersion::V2_4_0, false),
        (KotlinVersion::V2_5_0_RC, true),
    ])
}

/// From 2.5.0-RC kotlinc binds the generated `equals` of a data class, data object and value class
/// to its own class under `StrictEquals`; 2.4 leaves it unbound.
fn generated_equals_is_bound() -> bool {
    use krusty::kotlin_version::KotlinVersion;
    common::for_reference(&[
        (KotlinVersion::V2_4_0, false),
        (KotlinVersion::V2_5_0_RC, true),
    ])
}

/// An equality that holds with the generated `equals` as the selected call proves the other
/// operand has the receiver's class. A nullable receiver proves only the nullable class for a
/// nullable operand, and calling `equals` directly narrows as `==` does.
fn generated_bound_narrows() -> String {
    krusty::conformance::prepare_test_source(
        include_str!("box_regressions/StrictEqualsGeneratedBound.kt"),
        krusty::conformance::TestTarget::Jvm,
    )
}

#[test]
fn a_generated_equals_bound_narrows_the_other_operand() {
    let source = generated_bound_narrows();
    let reference_args = common::language_directives::kotlinc_args(&source);
    if generated_equals_is_bound() {
        common::assert_sources_accepted_like_kotlinc(&[("main.kt", &source)], &reference_args);
        assert_ok("GeneratedBoundNarrows", &source);
    } else {
        common::assert_errors_match_kotlinc(&[("main.kt", &source)], &reference_args);
    }
}

/// Only the bound of the selected `equals` narrows its argument: `Any.equals` selected for an
/// `Any` receiver, a user-declared override, and a dependency's data class compiled without
/// `StrictEquals` leave the other operand an `Any` on every release.
const UNBOUND_EQUALS_KEEPS_DECLARED_TYPE: &str = "\
// LANGUAGE: +StrictEquals
data class Point(val x: Int)
data class Custom(val c: Int) {
    override fun equals(other: Any?): Boolean = other is Custom && other.c == c
    override fun hashCode(): Int = c
}

fun anyReceiver(a: Any): Int { if (a != Point(1)) return 0; return a.x }
fun custom(a: Any): Int { if (Custom(1) != a) return 0; return a.c }
fun pair(a: Any): Int { if (Pair(1, 2) != a) return 0; return a.first as Int }
";

#[test]
fn an_unbound_equals_keeps_the_declared_type() {
    common::assert_errors_match_kotlinc(
        &[("main.kt", UNBOUND_EQUALS_KEEPS_DECLARED_TYPE)],
        &common::language_directives::kotlinc_args(UNBOUND_EQUALS_KEEPS_DECLARED_TYPE),
    );
}

/// `Any.equals` selected for an `Any` receiver proves nothing about its argument, even when that
/// argument's class declares an equality bound. kotlinc 2.4 rejects `@EqualityBound`, so the
/// comparison with kotlinc runs on the 2.5 release.
const ANY_RECEIVER_OF_A_BOUND_CLASS: &str = "\
// LANGUAGE: +StrictEquals
open class Base(val n: Int) {
    override fun equals(@EqualityBound(Base::class) other: Any?): Boolean = n == other.n
}

fun anyReceiver(a: Any): Int { if (a == Base(1)) return a.n; return 0 }
";

#[test]
fn an_any_receiver_does_not_narrow_by_its_arguments_bound() {
    if !kotlinc_admits_equality_bound() {
        return;
    }
    common::assert_errors_match_kotlinc(
        &[("main.kt", ANY_RECEIVER_OF_A_BOUND_CLASS)],
        &common::language_directives::kotlinc_args(ANY_RECEIVER_OF_A_BOUND_CLASS),
    );
}

/// The generated bound crosses a module boundary in `@Metadata`: kotlinc and krusty both narrow by
/// a krusty-built library's data class when the consumer enables `StrictEquals`.
const LIBRARY_DATA_CLASS: &str = "\
// LANGUAGE: +StrictEquals
package lib
data class Point(val x: Int)
";

const LIBRARY_DATA_CLASS_CONSUMER: &str = "\
// LANGUAGE: +StrictEquals
import lib.Point
fun point(a: Any): Int { if (Point(1) != a) return 0; return a.x }
";

#[test]
fn a_library_generated_equals_bound_narrows_in_the_consumer() {
    let library = common::compile_libs_build("LibraryDataClass", &[("Lib.kt", LIBRARY_DATA_CLASS)])
        .expect("krusty builds the library module");
    let reference_args = common::language_directives::kotlinc_args(LIBRARY_DATA_CLASS_CONSUMER);
    let result = common::compiler_diagnostics_with_reference_args(
        &[("main.kt", LIBRARY_DATA_CLASS_CONSUMER)],
        &[library.krusty_out().to_path_buf()],
        &reference_args,
    );
    let ledger = common::render_errors(&result.reference_stderr);
    assert_eq!(
        common::render_errors(&result.krusty_stderr),
        ledger,
        "krusty's ledger against kotlinc",
    );
    // 2.4 binds nothing, so both report the unresolved `x` on `Any`.
    let narrows = generated_equals_is_bound();
    assert_eq!(
        ledger.is_empty(),
        narrows,
        "kotlinc: {}",
        result.reference_stderr
    );
    assert_eq!(
        result.krusty_code == 0,
        narrows,
        "krusty: {}",
        result.krusty_stderr
    );
}
