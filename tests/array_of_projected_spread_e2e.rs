//! A spread argument of `arrayOf` accepts an out-projected array, as for any other vararg call.
//!
//! The program runs on the JVM against kotlinc. The Wasm KLIB lanes decline every array literal
//! with a spread, so a backend-neutral box regression cannot cover this resolution yet.
use super::common;

const PROGRAM: &str = include_str!("fixtures/array_of_projected_spread/ArrayOfProjectedSpread.kt");

#[test]
fn array_of_spreads_out_projected_arrays_like_kotlinc() {
    common::expect_box_same_as_kotlinc(PROGRAM, "ArrayOfProjectedSpread");
}
