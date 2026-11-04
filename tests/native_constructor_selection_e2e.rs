//! Strict Native guards for the shared constructor-selection programs.

use super::common;

#[test]
fn known_primary_selection_survives_physical_parameter_normalization() {
    let source = include_str!("box_regressions/regressions/constructorPrimaryParameterRoles.kt");
    common::expect_box_ok_with_stdlib(source, "NativePrimaryConstructorRoles");
    common::expect_native_box(source, "NativePrimaryConstructorRoles", "OK");
}

#[test]
fn ordinary_primary_and_explicit_secondary_selections_keep_their_roles() {
    let source = include_str!("box_regressions/regressions/constructorOrdinaryParameterRoles.kt");
    common::expect_box_ok_with_stdlib(source, "NativeOrdinaryConstructorRoles");
    common::expect_native_box(source, "NativeOrdinaryConstructorRoles", "OK");
}
