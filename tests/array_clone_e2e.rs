//! `Array<T>.clone()` returns the receiver's own application: `Array<String?>.clone()` is an
//! `Array<String?>`, as moshi's `JsonReader` copy constructor relies on.

use super::common;

const NULLABLE_ELEMENTS: &str = include_str!("box_regressions/arrayCloneNullableElements.kt");

#[test]
fn array_clone_keeps_nullable_element_types() {
    common::expect_box_same_as_kotlinc(NULLABLE_ELEMENTS, "ArrayClone");
}
