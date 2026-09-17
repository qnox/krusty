//! `x in range` where `x` is not the range's element type.
//!
//! Kotlin declares those as extensions on the ranges FACADE — `RangesKt.contains(IntRange, Long)` —
//! so the owner names no range type and the element has to come from the RECEIVER. The backend
//! derived it from the owner and declined every one by name.
//!
//! The argument is carried at ITS OWN width and never coerced to the element, which is the whole of
//! the correctness: `4294967296L in 0..5` is `false`, and truncating that `Long` to an `Int` makes
//! it 0 and answers `true`. The runtime keeps every bound at 64 bits and reads them at the range's
//! own signedness, so a value widened by its own signedness is already the comparison Kotlin
//! specifies.
//!
//! Every expectation is kotlinc's, taken by running the same expressions under it.

use super::common::{expect_native_box, expect_native_decline};

/// The cross-type comparisons, including the two that only a non-truncating one gets right.
#[test]
fn a_value_of_another_type_is_tested_against_a_range_at_its_own_width() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val r = 1..3\n\
         \x20   val z = 0..5\n\
         \x20   if ((-1L) in r) return \"fail -1L\"\n\
         \x20   if (!(2L in r)) return \"fail 2L\"\n\
         \x20   if ((-1).toByte() in r) return \"fail (-1).toByte()\"\n\
         \x20   if (!(2.toByte() in r)) return \"fail 2.toByte()\"\n\
         \x20   if (!(2.toShort() in r)) return \"fail 2.toShort()\"\n\
         \x20   if (!(2 in 1L..3L)) return \"fail Int in LongRange\"\n\
         \x20   if (!(2.toByte() in 1L..3L)) return \"fail Byte in LongRange\"\n\
         \x20   // 2^32 truncates to 0, which IS in 0..5 — so a narrowing comparison says true.\n\
         \x20   if (4294967296L in z) return \"fail 2^32\"\n\
         \x20   if (Long.MAX_VALUE in r) return \"fail Long.MAX_VALUE\"\n\
         \x20   if (Long.MIN_VALUE in r) return \"fail Long.MIN_VALUE\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "RangeContains",
        "OK",
    );
}

/// A PROGRESSION still declines, and that is a rule rather than an omission.
///
/// Kotlin answers `x in progression` by WALKING it, so `5 in (10 downTo 1 step 2)` is false though
/// 5 lies between the ends. A bounds test says true — and for a descending progression it compares
/// the wrong way round as well, since `first` is the larger end. Routing progressions to the
/// runtime's bounds test turned six declining corpus cases into WRONG answers, which is how this
/// boundary was found; the walk is its own change.
#[test]
fn a_progression_still_declines_because_membership_is_a_walk() {
    expect_native_decline(
        "fun box(): String {\n\
         \x20   val p = 10 downTo 1 step 2\n\
         \x20   return if (5L in p) \"bad\" else \"OK\"\n\
         }\n",
        "ProgressionContains",
        "contains",
    );
}

/// A NULLABLE value still declines. `null in 1..3` is false in Kotlin, which is a null test before
/// the comparison; reading the reference as a scalar without it dereferences null, and the corpus's
/// `nullableInPrimitiveRange.kt` segfaulted before this declined instead.
#[test]
fn a_nullable_value_still_declines_because_it_needs_a_null_test() {
    expect_native_decline(
        "fun box(): String {\n\
         \x20   val v: Long? = null\n\
         \x20   return if (v in 1..3) \"bad\" else \"OK\"\n\
         }\n",
        "NullableContains",
        "contains",
    );
}
