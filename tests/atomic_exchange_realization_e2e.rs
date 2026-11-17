//! Every Kotlin atomic exchange declaration uses the stdlib's JVM8-compatible helper, including
//! when the class-file target is omitted. The portable inventory owns the complete source.
use super::common;

const PROGRAM: &str = include_str!("box_regressions/atomicCompareExchange.kt");

#[test]
fn all_atomic_exchange_overloads_match_kotlinc_at_default_and_explicit_jvm8() {
    for target in [None, Some(8)] {
        let classes = common::classes_against_kotlinc_lib_target(
            "AtomicExchange",
            &[("Dependency.kt", "class Dependency\n")],
            PROGRAM,
            target,
        )
        .expect("reference toolchain required");
        assert_eq!(
            classes.reference.keys().collect::<Vec<_>>(),
            classes.krusty.keys().collect::<Vec<_>>()
        );
        for bytes in classes.reference.values().chain(classes.krusty.values()) {
            assert_eq!(u16::from_be_bytes([bytes[6], bytes[7]]), 52);
        }
        for declaration in [
            "public static final int exchangeInt(java.util.concurrent.atomic.AtomicInteger, int, int);",
            "public static final long exchangeLong(java.util.concurrent.atomic.AtomicLong, long, long);",
            "public static final boolean exchangeBoolean(java.util.concurrent.atomic.AtomicBoolean, boolean, boolean);",
            "public static final java.lang.String exchangeReference(java.util.concurrent.atomic.AtomicReference<java.lang.String>, java.lang.String, java.lang.String);",
            "public static final int exchangeIntArray(java.util.concurrent.atomic.AtomicIntegerArray, int, int, int);",
            "public static final long exchangeLongArray(java.util.concurrent.atomic.AtomicLongArray, int, long, long);",
            "public static final java.lang.String exchangeArray(java.util.concurrent.atomic.AtomicReferenceArray<java.lang.String>, int, java.lang.String, java.lang.String);",
        ] {
            let (reference, actual) = classes.method_listing("AtomicExchangeKt", declaration);
            assert_eq!(actual, reference, "{target:?}: {declaration}");
        }
    }
    common::expect_box_ok_with_stdlib(PROGRAM, "AtomicExchange");
}
