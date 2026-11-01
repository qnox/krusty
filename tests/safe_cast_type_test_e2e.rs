//! A non-null safe cast proves the type test, so its operand smart-casts after each form.
//!
//! This is the complete program, run against kotlinc. It throws through an Elvis `error()`,
//! rejects null with an exact exception message and reads the smart cast inside `listOf`/`map`
//! callbacks. The Wasm and Native KLIB lanes cannot realize those stdlib bodies yet, so the box
//! regression `safeCastProvesTypeTest.kt` repeats the resolution over local declarations on every
//! target.
use super::common;

const PROGRAM: &str = include_str!("fixtures/safe_cast_type_test/SafeCastProvesTypeTest.kt");

#[test]
fn safe_cast_operand_smart_casts_like_kotlinc() {
    common::expect_box_same_as_kotlinc(PROGRAM, "SafeCastProvesTypeTest");
}
