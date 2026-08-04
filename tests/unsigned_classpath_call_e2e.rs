//! Unsigned values crossing a CLASSPATH call boundary, where the two sides disagree about whether
//! the value is BOXED.
//!
//! krusty carries `UInt`/`ULong` in the JVM primitive slot of their carrier (`I`/`J`) and boxes to
//! `kotlin/UInt`/`kotlin/ULong` only where a reference is required. Every classpath call is therefore
//! a place where the representation the lowerer produced has to agree with the descriptor the
//! backend spells verbatim. Two shapes disagreed and emitted a class that failed JVM verification
//! while krusty reported success:
//!
//! - `maxOf(a, b)` selects the value-class-mangled `maxOf-J1ME1BU:(II)I`, whose descriptor carries
//!   the ERASED carrier — but the parameter was recovered as the boxed metadata class, so the
//!   argument coercion boxed the carrier into a slot that takes it unboxed;
//! - `a.equals(b)` is an `invokevirtual` on `kotlin/UInt`, which needs the BOXED receiver — but the
//!   receiver was pushed as the raw carrier.
//!
//! `jvm_can_emit` could not catch either: it inspects the TYPES a file mentions, and `kotlin/UInt`
//! is a fully supported type there. What was wrong was the representation of a value at a call
//! boundary, which no signature-level check can see.
//!
//! Both now emit kotlinc's shapes, and a SAME-TYPE `a.equals(b)` no longer reaches the member call
//! at all — it folds to a carrier compare (kotlinc's intrinsic; the shape is pinned in
//! `bytecode_parity_e2e`). The boxed receiver still carries every other argument type, which is what
//! `unsigned_equals_keeps_value_class_semantics_across_argument_types` exercises.
//!
//! The contract pinned here is the backend's, not the feature's. Declining the file is always a
//! legal outcome; claiming success and writing a class that fails verification is not. So
//! [`expect_emitted_box_verifies`] accepts a backend decline and fails only when krusty says it
//! emitted a class and that class does not load and run.

use super::common;
use super::common::BackendOutcome;

/// Assert the backend's own contract: whatever krusty EMITS must verify and run.
///
/// A decline (any lowering/backend bail) passes — an unsupported construct is allowed to make the
/// file skip. Only "krusty emitted a class file, and the JVM rejects it" fails. A front-end
/// rejection fails too: these are backend tests and must not pass through a parse/type error.
fn expect_emitted_box_verifies(src: &str, stem: &str) {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    let cp = std::slice::from_ref(&stdlib);
    match common::backend_outcome_in_process(src, stem, cp, Some(&jdk)) {
        None => panic!("{stem}: the front end rejected the source; this is a backend test"),
        Some(BackendOutcome::Emitted) => {
            let Some(out) = common::compile_and_run_box(src, stem, cp, Some(&jdk)) else {
                panic!(
                    "{stem}: krusty reported success, but the emitted class does not load and run \
                     (a class that fails verification is strictly worse than declining the file)"
                );
            };
            assert_eq!(out, "OK", "{stem}");
        }
        // Declined: the file skips. Always allowed.
        Some(_) => {}
    }
}

/// `maxOf` on an unsigned pair — the mangled `maxOf-J1ME1BU:(II)I` / `maxOf-eb3DHEI:(JJ)J`.
#[test]
fn unsigned_max_of_emits_verifiable_bytecode() {
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UInt = 200u\n\
    val b: UInt = 100u\n\
    return if (maxOf(a, b) == a) \"OK\" else \"bad\"\n\
}\n",
        "UMaxOf",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: ULong = 200uL\n\
    val b: ULong = 100uL\n\
    return if (maxOf(a, b) == a) \"OK\" else \"bad\"\n\
}\n",
        "ULongMaxOf",
    );
}

/// The `minOf` sibling — same selection, same erased descriptor.
#[test]
fn unsigned_min_of_emits_verifiable_bytecode() {
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UInt = 200u\n\
    val b: UInt = 100u\n\
    return if (minOf(a, b) == b) \"OK\" else \"bad\"\n\
}\n",
        "UMinOf",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: ULong = 200uL\n\
    val b: ULong = 100uL\n\
    return if (minOf(a, b) == b) \"OK\" else \"bad\"\n\
}\n",
        "ULongMinOf",
    );
}

/// `a.equals(b)`. A same-type pair folds to a carrier compare (kotlinc's intrinsic — shape pinned in
/// `bytecode_parity_e2e`); any other argument stays an `invokevirtual kotlin/UInt.equals(Object)Z`,
/// whose receiver must be the BOXED value class. Both paths are exercised here.
#[test]
fn unsigned_equals_call_emits_verifiable_bytecode() {
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UInt = 200u\n\
    val b: UInt = 100u\n\
    if (a.equals(b)) return \"bad ne\"\n\
    if (!a.equals(a)) return \"bad eq\"\n\
    return \"OK\"\n\
}\n",
        "UEquals",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: ULong = 200uL\n\
    val b: ULong = 100uL\n\
    if (a.equals(b)) return \"bad ne\"\n\
    if (!a.equals(a)) return \"bad eq\"\n\
    return \"OK\"\n\
}\n",
        "ULongEquals",
    );
}

/// `UByte`/`UShort` have no carrier `Ty` of their own and are declined by `jvm_can_emit`'s
/// unsupported-value-class list. Pinned so the decline stays a DECLINE if that list ever moves —
/// the same shapes must never start emitting instead.
#[test]
fn narrow_unsigned_types_stay_declined_or_correct() {
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UByte = 200.toUByte()\n\
    val b: UByte = 100.toUByte()\n\
    return if (maxOf(a, b) == a) \"OK\" else \"bad\"\n\
}\n",
        "UByteMaxOf",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UShort = 200.toUShort()\n\
    val b: UShort = 100.toUShort()\n\
    return if (a.equals(b)) \"bad\" else \"OK\"\n\
}\n",
        "UShortEquals",
    );
}

/// The `equals` intrinsic must not change what `equals` MEANS. kotlinc folds only the same-type case
/// to a carrier compare; every other argument still goes through the value class's own equality,
/// which is what makes a cross-carrier comparison answer `false` (a `UInt` is never a `ULong`, even
/// when the carriers hold the same bits).
#[test]
fn unsigned_equals_keeps_value_class_semantics_across_argument_types() {
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: UInt = 4294967295u\n\
    val b: UInt = 1u\n\
    if (a.equals(b)) return \"f same-ne\"\n\
    if (!a.equals(a)) return \"f same-eq\"\n\
    if (a.equals(4294967295uL)) return \"f cross-carrier\"\n\
    val anyA: Any = a\n\
    if (!a.equals(anyA)) return \"f any-eq\"\n\
    if (a.equals(\"4294967295\")) return \"f any-ne\"\n\
    return \"OK\"\n\
}\n",
        "UEqualsSemantics",
    );
    expect_emitted_box_verifies(
        "fun box(): String {\n\
    val a: ULong = 18446744073709551615uL\n\
    val b: ULong = 1uL\n\
    if (a.equals(b)) return \"f same-ne\"\n\
    if (!a.equals(a)) return \"f same-eq\"\n\
    val anyA: Any = a\n\
    if (!a.equals(anyA)) return \"f any-eq\"\n\
    return \"OK\"\n\
}\n",
        "ULongEqualsSemantics",
    );
}
