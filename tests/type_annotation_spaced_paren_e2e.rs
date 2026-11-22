//! Whitespace before `(` after a type-use annotation is significant (kotlinc's
//! `WITH_SIGNIFICANT_WHITESPACE_BEFORE_ARGUMENTS`): `@Tag (() -> String)?` annotates a
//! parenthesized nullable function type instead of passing `() -> String` as annotation arguments.
//! The shared regression program is compiled by kotlinc and krusty and must run identically.

use super::common;

#[test]
fn spaced_parenthesized_function_type_is_annotated_not_called() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/SpacedTypeAnnotationParenthesizedFunctionType.kt"),
        "SpacedTypeAnnotationParenthesizedFunctionTypeRun",
    );
}
