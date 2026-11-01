//! Main integration-test target. Cargo builds each top-level tests/*.rs file as a separate
//! crate, so product e2e modules live behind one target to keep link time and target size bounded.
//! External corpus/reference-toolchain suites live in `conformance.rs`.
//!
//! Every `tests/*_e2e.rs` file is one module of this target, declared by `declare_test_modules!`:
//! adding a test file needs no edit here. Only shared support and the few suites without the
//! suffix are declared by hand below.

krusty_test_modules::declare_test_modules!("_e2e.rs");

#[path = "common/bytecode_comparison.rs"]
mod bytecode_comparison_support;
#[path = "common/mod.rs"]
mod common_core;
#[cfg(test)]
pub(crate) use common_core::server_pool::Pool as ServerPool;
#[path = "common/compiler_plugin_fixture.rs"]
mod compiler_plugin_fixture;
#[path = "common/e2e.rs"]
mod e2e_support;
#[path = "common/inner_class_rows.rs"]
mod inner_class_rows_support;
#[path = "common/reference_box.rs"]
mod reference_box_support;
#[path = "common/reference_compiler.rs"]
mod reference_compiler_support;
#[path = "common/reference_recording.rs"]
mod reference_recording_support;
#[path = "common/reference_targets.rs"]
mod reference_targets_support;
#[path = "common/target_diagnostics.rs"]
mod target_diagnostics_support;

mod common {
    pub use super::bytecode_comparison_support::*;
    pub use super::common_core::*;
    pub use super::e2e_support::*;
    pub use super::inner_class_rows_support::{
        assert_same_inner_classes, compile_with_kotlinc, compile_with_kotlinc_for_target,
    };
    pub use super::reference_box_support::*;
    pub use super::reference_compiler_support::*;
    pub use super::reference_targets_support::*;
    pub use super::target_diagnostics_support::*;
}

#[path = "class_metadata_roundtrip.rs"]
mod class_metadata_roundtrip;
#[path = "classpath_companion.rs"]
mod classpath_companion;
#[path = "deep_expression_nesting_check.rs"]
mod deep_expression_nesting_check;
#[path = "dep_resolution.rs"]
mod dep_resolution;
#[path = "diagnostics_parity_support.rs"]
mod diagnostics_parity_support;
#[path = "generic_hof_method_check.rs"]
mod generic_hof_method_check;
#[path = "metadata_kept_params.rs"]
mod metadata_kept_params;
#[path = "metadata_return_types.rs"]
mod metadata_return_types;

#[path = "common/serialization_runtime.rs"]
mod serialization_runtime_support;
#[path = "common/serialization.rs"]
mod serialization_test_support;

