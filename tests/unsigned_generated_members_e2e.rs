//! Members kotlinc generates over an unsigned property or constructor parameter.
//!
//! An unsigned type is an inline class over its signed carrier, so kotlinc treats it the way it
//! treats any value class:
//!
//! - a value class over `UInt` compares through `UInt.equals-impl0` and renders through
//!   `UInt.toString-impl`, appending the text as an object;
//! - a data class hashes a `UInt?` property by unboxing it after the null test, then calling
//!   `UInt.hashCode-impl`, and its `@Metadata` records a component's descriptor exactly when a
//!   reader cannot rebuild it (`component1(): UInt` is `()I`, `component2(): UInt?` is not);
//! - a value class over another value class compares through that class's `equals-impl0`;
//! - a class whose constructor takes a non-null or nullable unsigned parameter declares that
//!   constructor private and adds a public one with a trailing `DefaultConstructorMarker`, which
//!   every call site uses;
//! - a value class over `ULong?` passes the box to its static members, and its `constructor-impl`,
//!   like any nullable carrier's, takes a `@Nullable` value and returns a `@NotNull` one.
//!
//! These used to differ from kotlinc, and a value class over `UInt` failed verification at run
//! time.
use super::common;

const SRC: &str = "package t\n\
    \n\
    fun f(u: UInt, s: String) = \"a${u}b$s\"\n\
    data class D(val u: UInt, val n: UInt?)\n\
    @JvmInline value class V(val u: UInt)\n\
    @JvmInline value class Raw(val i: Int)\n\
    @JvmInline value class Tag(val raw: Raw)\n\
    @JvmInline value class W(val u: ULong?)\n\
    @JvmInline value class Text(val s: String?)\n\
    class P(val u: UInt)\n\
    class Q(val u: UInt?)\n\
    class S(val s: String) {\n\
    \x20   constructor(u: UInt) : this(u.toString())\n\
    }\n\
    \n\
    fun box(): String {\n\
    \x20   if (V(UInt.MAX_VALUE).toString() != \"V(u=4294967295)\") return \"V.toString\"\n\
    \x20   if (V(1u) != V(1u) || V(1u) == V(2u)) return \"V.equals\"\n\
    \x20   if (V(5u).hashCode() != 5) return \"V.hashCode\"\n\
    \x20   if (W(ULong.MAX_VALUE).toString() != \"W(u=18446744073709551615)\") return \"W.toString\"\n\
    \x20   if (W(null).hashCode() != 0 || W(3uL) != W(3uL) || W(3uL).u != 3uL) return \"W\"\n\
    \x20   if (Text(null).s != null || Text(\"a\") != Text(\"a\")) return \"Text\"\n\
    \x20   if (Tag(Raw(1)) != Tag(Raw(1)) || Tag(Raw(1)) == Tag(Raw(2))) return \"Tag.equals\"\n\
    \x20   if (D(UInt.MAX_VALUE, 3u).toString() != \"D(u=4294967295, n=3)\") return \"D.toString\"\n\
    \x20   if (D(1u, 2u).hashCode() != 33) return \"D.hashCode\"\n\
    \x20   if (D(1u, null).hashCode() != 31) return \"D.hashCode null\"\n\
    \x20   if (f(UInt.MAX_VALUE, \"x\") != \"a4294967295bx\") return \"template\"\n\
    \x20   if (P(3u).u != 3u || Q(null).u != null || S(4u).s != \"4\") return \"constructors\"\n\
    \x20   return \"OK\"\n\
    }\n";

fn assert_byte_identical(class: &str) {
    match common::byte_diff_against_kotlinc_cp(
        "UnsignedMembers",
        SRC,
        class,
        &[common::stdlib_jar()],
    ) {
        None => panic!("{class}: reference toolchain unavailable"),
        Some(Ok(())) => {}
        Some(Err(e)) => panic!("{e}"),
    }
}

/// The native backend renders a packaged value class under its simple name (`V(u=…)`), as the
/// JVM does, so both backends pass every check.
#[test]
fn unsigned_generated_members_run() {
    common::expect_box_ok_with_stdlib(SRC, "UnsignedMembers");
}

#[test]
fn a_value_class_over_an_unsigned_type_matches_kotlinc() {
    assert_byte_identical("t/V");
}

#[test]
fn a_value_class_over_a_value_class_matches_kotlinc() {
    assert_byte_identical("t/Tag");
}

#[test]
fn a_data_class_over_unsigned_properties_matches_kotlinc() {
    assert_byte_identical("t/D");
}

#[test]
fn an_unsigned_constructor_parameter_hides_the_constructor() {
    assert_byte_identical("t/P");
    assert_byte_identical("t/Q");
    assert_byte_identical("t/S");
}

#[test]
fn an_unsigned_template_and_constructor_calls_match_kotlinc() {
    assert_byte_identical("t/UnsignedMembersKt");
}

/// The `ULong?` carrier is a reference: no member unboxes it before passing it on.
#[test]
fn a_value_class_over_a_nullable_unsigned_type_matches_kotlinc() {
    assert_byte_identical("t/W");
}

#[test]
fn a_value_class_over_a_nullable_reference_matches_kotlinc() {
    assert_byte_identical("t/Text");
}
