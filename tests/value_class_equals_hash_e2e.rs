//! A value class's generated `equals-impl(U, Object)` and `hashCode-impl(U)` treat the sole
//! property exactly as a data class treats one of its properties, as kotlinc's generator does.
//!
//! The hash is the declared type's own: `String.hashCode()`, a wrapper's static `hashCode`,
//! `Arrays.hashCode` (its `Object[]` overload for any reference array), a nested value class's `hashCode-impl`. A type that admits null (`String?`,
//! `Int?`, `IntArray?`, an unbounded `T`) hashes behind `v == null ? 0 : …`, never through
//! `Objects.hashCode`, and a nullable primitive is never unboxed for it. Equality unboxes the other
//! value into a temporary first and compares `arg0` with it through the declared type's equality,
//! so a reference temporary stays on the stack (`aload_0; swap`). A nested value class is called
//! with its own carrier, which stops at a nullable value class the JVM keeps boxed (`Count?`).

use super::common;

const SRC: &str = "@JvmInline value class Label(val text: String)\n\
    open class Owned(val id: Int)\n\
    @JvmInline value class OwnedId(val id: Int)\n\
    @JvmInline value class MaybeLabel(val text: String?)\n\
    @JvmInline value class MaybeCount(val count: Int?)\n\
    @JvmInline value class Anything(val value: Any?)\n\
    @JvmInline value class Slot<T>(val value: T)\n\
    @JvmInline value class Present<T : Owned>(val value: T)\n\
    @JvmInline value class Chained<T : U, U : Owned>(val value: T)\n\
    @JvmInline value class Cells(val cells: IntArray)\n\
    @JvmInline value class MaybeCells(val cells: IntArray?)\n\
    @JvmInline value class Wrapped(val inner: MaybeLabel)\n\
    @JvmInline value class Count(val count: Int)\n\
    @JvmInline value class OwnedValues(val values: Array<OwnedId>)\n\
    @JvmInline value class OwnedRefs(val values: Array<Owned>?)\n\
    @JvmInline value class MaybeCountBox(val inner: Count?)\n\
    @JvmInline value class NestedBox(val inner: MaybeCountBox)\n\
    fun box(): String {\n\
    \x20   if (MaybeCount(null).hashCode() != 0) return \"nullable Int\"\n\
    \x20   if (MaybeLabel(null).hashCode() != 0) return \"nullable String\"\n\
    \x20   if (MaybeCells(null).hashCode() != 0) return \"nullable array\"\n\
    \x20   if (OwnedRefs(null).hashCode() != 0) return \"nullable reference array\"\n\
    \x20   if (NestedBox(MaybeCountBox(null)).hashCode() != 0) return \"nested boxed hash\"\n\
    \x20   val n: Any = NestedBox(MaybeCountBox(Count(1)))\n\
    \x20   if (n != NestedBox(MaybeCountBox(Count(1)))) return \"nested boxed equals\"\n\
    \x20   if (Slot<Any?>(null).hashCode() != 0) return \"generic\"\n\
    \x20   val owned = Owned(7)\n\
    \x20   val chained: Any = Chained<Owned, Owned>(owned)\n\
    \x20   if (chained != Chained<Owned, Owned>(owned)) return \"bound chain equals\"\n\
    \x20   if (chained.hashCode() != owned.hashCode()) return \"bound chain hash\"\n\
    \x20   val a: Any = Anything(\"x\")\n\
    \x20   if (a != Anything(\"x\")) return \"equals\"\n\
    \x20   if (a == Anything(null)) return \"not equals\"\n\
    \x20   val w: Any = Wrapped(MaybeLabel(\"y\"))\n\
    \x20   if (w != Wrapped(MaybeLabel(\"y\"))) return \"nested\"\n\
    \x20   return \"OK\"\n\
    }\n";

/// `class`'s `member` instructions, call targets included, match kotlinc's.
fn assert_same_instructions(class: &str, member: &str) {
    let built = common::compare_with_kotlinc_plugin(
        "ValueClassEqualsHash",
        SRC,
        class,
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let reference = common::method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc emits {class}.{member}");
    assert_eq!(
        common::method_instructions(&built.krusty, member),
        reference,
        "{class}.{member}"
    );
}

const CLASSES: [(&str, &str); 15] = [
    ("Label", "java.lang.String"),
    ("MaybeLabel", "java.lang.String"),
    ("MaybeCount", "java.lang.Integer"),
    ("Anything", "java.lang.Object"),
    ("Slot", "java.lang.Object"),
    ("Present", "Owned"),
    ("Chained", "Owned"),
    ("Cells", "int[]"),
    ("MaybeCells", "int[]"),
    ("Wrapped", "java.lang.String"),
    ("Count", "int"),
    ("OwnedValues", "OwnedId[]"),
    ("OwnedRefs", "Owned[]"),
    ("MaybeCountBox", "Count"),
    ("NestedBox", "Count"),
];

#[test]
fn hash_code_impl_hashes_the_declared_type_like_kotlinc() {
    for (class, carrier) in CLASSES {
        assert_same_instructions(class, &format!("static int hashCode-impl({carrier});"));
    }
}

#[test]
fn equals_impl_compares_through_a_temporary_like_kotlinc() {
    for (class, carrier) in CLASSES {
        assert_same_instructions(
            class,
            &format!("static boolean equals-impl({carrier}, java.lang.Object);"),
        );
    }
}

#[test]
fn value_class_equality_and_hashes_run() {
    common::expect_box_ok_with_stdlib(SRC, "ValueClassEqualsHash");
}

/// The generator is shared: a data class's nullable array property also hashes by content,
/// `Arrays.hashCode` behind the null check, not the array's identity `hashCode()`, and a reference
/// array of a value class takes the `Object[]` overload.
#[test]
fn a_data_class_hashes_a_nullable_array_by_content() {
    let src = "data class Bag(val cells: IntArray?, val name: String?, val tags: Array<UInt>)\n";
    let built = common::compare_with_kotlinc_plugin(
        "DataClassNullableArrayHash",
        src,
        "Bag",
        &[common::stdlib_jar()],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    let member = "public int hashCode();";
    let reference = common::method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "kotlinc emits Bag.hashCode");
    assert_eq!(
        common::method_instructions(&built.krusty, member),
        reference
    );
}
