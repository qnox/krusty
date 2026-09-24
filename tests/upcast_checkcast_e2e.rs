//! A value materialized at a supertype is cast to it, as kotlinc does.
//!
//! kotlinc materializes a value at the type its consumer declares, and between two different
//! reference types that is a `checkcast` to the consumer's type (`StackValue.coerce`) — an upcast
//! included. Only `java/lang/Object` is never cast to. krusty cast only an erased `Object`, so a
//! `String` passed as a `CharSequence`, or a subclass returned as its base, had no `checkcast`.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};
use super::serialization_companion_byte_parity_e2e::compare_files_with_kotlinc_plugin;

const SOURCE: &str = "open class Base\n\
    class Derived : Base()\n\
    interface Shape\n\
    class Square : Shape\n\
    fun base(value: Base): Int = 1\n\
    fun shape(value: Shape): Int = 2\n\
    fun text(value: CharSequence): Int = value.length\n\
    fun anything(value: Any): Int = 3\n\
    fun baseDefault(value: Base, tag: Int = 5): Int = tag\n\
    fun toBase(value: Derived): Int = base(value)\n\
    fun toBaseDefault(value: Derived): Int = baseDefault(value)\n\
    fun toShape(value: Square): Int = shape(value)\n\
    fun toText(value: String): Int = text(value)\n\
    fun toAnything(value: String): Int = anything(value)\n\
    fun widened(value: String): CharSequence = value\n\
    class Holder { fun take(value: Base): Int = 4 }\n\
    fun member(holder: Holder, value: Derived): Int = holder.take(value)\n";

const MEMBERS: [&str; 7] = [
    "int toBase(",
    "int toBaseDefault(",
    "int toShape(",
    "int toText(",
    "int toAnything(",
    "java.lang.CharSequence widened(",
    "int member(",
];

#[test]
fn a_value_widened_to_its_consumers_type_is_cast_like_kotlincs() {
    let Some(built) = compare_with_kotlinc_plugin(
        "UpcastCheckcast",
        SOURCE,
        "UpcastCheckcastKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in MEMBERS {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

/// The general consumption coercion must retain kotlinc's explicit no-cast cases. These source
/// forms pin null, a diverging value, and JVM array covariance independently from ordinary class
/// upcasts.
#[test]
fn null_bottom_and_covariant_array_do_not_gain_spurious_casts() {
    const EXCEPTIONS: &str = "open class BoundaryTarget\n\
        fun nullTarget(): BoundaryTarget? = null\n\
        fun thrownTarget(failure: Throwable): BoundaryTarget = throw failure\n\
        fun objectArray(value: Array<out String>): Array<out Any> = value\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "UpcastExceptions",
        EXCEPTIONS,
        "UpcastExceptionsKt",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    for member in [
        "BoundaryTarget nullTarget(",
        "BoundaryTarget thrownTarget(",
        "java.lang.Object[] objectArray(",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "{member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

/// Field storage is another JVM consumption boundary. This repository-owned class keeps the
/// regression independent of accessor naming or stdlib intrinsics: the write is a direct
/// `putfield` in the declaring class.
#[test]
fn a_field_write_casts_to_its_declared_storage_type_like_kotlinc() {
    const FIELD_SOURCE: &str = "open class StoredBase\n\
        class StoredDerived : StoredBase()\n\
        class Store(var stored: StoredBase) {\n\
        \x20   fun update(value: StoredDerived) { stored = value }\n\
        }\n";
    let Some(built) = compare_with_kotlinc_plugin(
        "FieldUpcastCheckcast",
        FIELD_SOURCE,
        "Store",
        &[common::stdlib_jar()],
        "25",
        &[],
    ) else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let member = "void update(";
    let reference = method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "{member} not found");
    assert_eq!(method_instructions(&built.krusty, member), reference);
}

#[test]
fn code_with_its_widening_casts_still_runs() {
    common::expect_box_ok_with_stdlib(
        &format!(
            "{SOURCE}\
             fun box(): String {{\n\
             \x20   val sum = toBase(Derived()) + toBaseDefault(Derived()) +\n\
             \x20       toShape(Square()) + toText(\"abc\") +\n\
             \x20       toAnything(\"x\") + widened(\"yz\").length + member(Holder(), Derived())\n\
             \x20   return if (sum == 20) \"OK\" else \"sum $sum\"\n\
             }}\n"
        ),
        "upcast checkcast",
    );
}

/// A declaration in another source file has a distinct source-module callee realization. Its
/// `$default` call must use the same consumption-type coercion as a same-file call; the checked
/// identity and realized descriptor are already recorded before emission.
#[test]
fn a_cross_file_default_call_casts_a_supplied_value_like_kotlinc() {
    let sources = [
        (
            "Target.kt",
            "open class CrossBase\n\
             class CrossDerived : CrossBase()\n\
             fun crossBase(value: CrossBase, tag: Int = 7): Int = tag\n",
        ),
        (
            "Caller.kt",
            "fun crossDefault(value: CrossDerived): Int = crossBase(value)\n",
        ),
    ];
    let Some(built) =
        compare_files_with_kotlinc_plugin(&sources, "CallerKt", &[common::stdlib_jar()], &[])
    else {
        eprintln!("skipping: reference kotlinc or javap unavailable");
        return;
    };
    let member = "int crossDefault(";
    let reference = method_instructions(&built.reference, member);
    assert!(!reference.is_empty(), "{member} not found");
    assert_eq!(method_instructions(&built.krusty, member), reference);
}
