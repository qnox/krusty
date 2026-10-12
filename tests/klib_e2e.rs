//! Focused e2e target consumed by the maximum-version Native conformance row.
//!
//! The ordinary `e2e` target remains the owner of the complete product suite. This opt-in target
//! reuses only the modules exercised by the KLIB/Native filters in CI, so building those fifteen
//! tests does not compile and link every unrelated e2e module first.

#[path = "common/mod.rs"]
mod common_core;
#[path = "common/e2e.rs"]
mod e2e_support;
#[path = "common/reference_compiler.rs"]
mod reference_compiler_support;
#[path = "common/reference_recording.rs"]
mod reference_recording_support;
#[path = "common/reference_targets.rs"]
mod reference_targets_support;
#[path = "common/serialization.rs"]
mod serialization_test_support;

mod common {
    pub use super::common_core::*;
    pub use super::e2e_support::*;
    pub use super::reference_compiler_support::*;
    pub use super::reference_targets_support::*;
}

#[path = "klib_class_surface_e2e.rs"]
mod klib_class_surface_e2e;
#[path = "klib_container_e2e.rs"]
mod klib_container_e2e;
#[path = "klib_semantic_e2e.rs"]
mod klib_semantic_e2e;
#[path = "native_value_classes_e2e.rs"]
mod native_value_classes_e2e;
#[path = "outcome_lists_guard_e2e.rs"]
mod outcome_lists_guard_e2e;
#[path = "value_class_declaration_e2e.rs"]
mod value_class_declaration_e2e;
