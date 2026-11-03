//! PLATFORM-SPECIFIC: Native physical constructor parameters and known-primary IR selections.
//!
//! The programs are shared portable box regressions; Native's physical constructor parameters
//! are why it gets this additional guard. Every expectation is kotlinc's.

use super::common::{expect_box_ok_with_stdlib, expect_native_box, kotlinc_box_result};

/// Require kotlinc's answer, krusty's JVM answer and the Native answer to agree.
fn every_backend_agrees_with_kotlinc(source: &str, stem: &str) {
    assert_eq!(
        kotlinc_box_result(source),
        "OK",
        "{stem}: unexpected kotlinc result"
    );
    expect_box_ok_with_stdlib(source, stem);
    expect_native_box(source, stem, "OK");
}

#[test]
fn known_primary_selection_survives_physical_parameter_normalization() {
    every_backend_agrees_with_kotlinc(
        include_str!("box_regressions/knownPrimaryConstructorRoles.kt"),
        "NativePrimaryConstructorRoles",
    );
}

#[test]
fn ordinary_primary_and_explicit_secondary_selections_keep_their_roles() {
    every_backend_agrees_with_kotlinc(
        include_str!("box_regressions/ordinaryAndSecondaryConstructorRoles.kt"),
        "NativeOrdinaryConstructorRoles",
    );
}
