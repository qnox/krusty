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

/// A PROGRESSION, whose membership is a walk rather than a span.
///
/// Kotlin answers `x in progression` by walking it, and the runtime answers the same question in
/// constant time: the value must lie between the ends IN THE WALK'S OWN DIRECTION, and it must sit
/// ON the step. `5 in (10 downTo 1 step 2)` is false though 5 lies between 10 and 2, because the
/// walk visits only 10, 8, 6, 4, 2 — a plain bounds test says true, which is why this declined
/// until the runtime read `step`.
///
/// A descending progression is the case that needs the direction: its `first` is ABOVE its `last`,
/// so the ascending test rejects every member it has. A plain range steps by 1 and satisfies the
/// step test for free, which is why extending the rule did not disturb it.
///
/// `last` is already the last element REACHED, so an empty range fails the bounds test for every
/// value and needs no case of its own.
#[test]
fn a_progression_is_walked_not_spanned() {
    expect_native_box(
        "fun box(): String {\n\
         \x20   val down = 10 downTo 1 step 2\n\
         \x20   val up = 1..10 step 3\n\
         \x20   val short = 1..9 step 3\n\
         \x20   if (!(6 in down)) return \"fail 6 in down\"\n\
         \x20   if (5 in down) return \"fail 5 between the ends but off the step\"\n\
         \x20   if (!(10 in down)) return \"fail first\"\n\
         \x20   if (!(2 in down)) return \"fail last\"\n\
         \x20   if (1 in down) return \"fail 1 past the last reached\"\n\
         \x20   if (11 in down) return \"fail above first\"\n\
         \x20   if (0 in down) return \"fail below last\"\n\
         \x20   if (!(4 in up)) return \"fail 4 in up\"\n\
         \x20   if (5 in up) return \"fail 5 off the step\"\n\
         \x20   if (!(10 in up)) return \"fail 10 reached exactly\"\n\
         \x20   if (11 in up) return \"fail above\"\n\
         \x20   // `1..9 step 3` reaches 7, not 9: `last` is normalized onto the step.\n\
         \x20   if (!(7 in short)) return \"fail 7 is the last reached\"\n\
         \x20   if (9 in short) return \"fail 9 is the bound, not an element\"\n\
         \x20   if (3 in 5..1) return \"fail empty range\"\n\
         \x20   return \"OK\"\n\
         }\n",
        "ProgressionContains",
        "OK",
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
