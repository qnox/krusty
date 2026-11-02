//! A destination producer is solved against the call's expected result.
//!
//! This is the complete program, run against kotlinc: `filterTo`, `mapTo` and `toCollection`
//! into `mutableSetOf()`/`mutableListOf()` with an expected `Base` collection. The Wasm and Native
//! KLIB lanes cannot realize those stdlib bodies yet, so the box regression
//! `destinationProducerExpectedResult.kt` repeats the inference over local declarations on every
//! target.
use super::common;

const PROGRAM: &str =
    include_str!("fixtures/destination_producer_result/DestinationProducerExpectedResult.kt");

#[test]
fn destination_producers_take_the_expected_result_like_kotlinc() {
    common::expect_box_same_as_kotlinc(PROGRAM, "DestinationProducerExpectedResult");
}
