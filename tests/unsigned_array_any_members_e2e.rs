//! An unsigned array's `toString` and `hashCode` in the shape of kotlinc's JVM backend.
//!
//! An unsigned array is a value class over the signed array of its width, so a call carries the
//! signed array unboxed. kotlinc realizes the generated `toString` and `hashCode` over that carrier
//! as the class's statics, `UIntArray.toString-impl([I)` and `UIntArray.hashCode-impl([I)`. A
//! virtual call on the box's `toString()` would dispatch on a `[I` the verifier cannot accept as a
//! `kotlin/UIntArray`. A template renders through the same static; an `Any`-typed value is boxed
//! and dispatches virtually.
//!
//! The program is the shared box regression, which every backend's conformance lane also runs.

use super::common;

const PROGRAM: &str = include_str!("box_regressions/unsignedArrayAnyMembers.kt");

#[test]
fn an_unsigned_array_renders_and_hashes_through_its_statics_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc(
        "UnsignedArrayAnyMembers",
        PROGRAM,
        &["UnsignedArrayAnyMembersKt"],
    );
}

#[test]
fn an_unsigned_array_renders_and_hashes_like_kotlinc() {
    common::expect_box_same_as_kotlinc(PROGRAM, "unsigned_array_any_members");
}
