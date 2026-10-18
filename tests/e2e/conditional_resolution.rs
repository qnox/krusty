//! Conditional-result inference and completion regressions.

pub(super) use super::common;

#[path = "../conditional_block_branch_rebind_e2e.rs"]
mod conditional_block_branch_rebind_e2e;
#[path = "../conditional_branch_expectation_e2e.rs"]
mod conditional_branch_expectation_e2e;
#[path = "../conditional_branch_inference_e2e.rs"]
mod conditional_branch_inference_e2e;
#[path = "../conditional_sibling_rebind_e2e.rs"]
mod conditional_sibling_rebind_e2e;
#[path = "../uninferred_constructor_type_parameter_e2e.rs"]
mod uninferred_constructor_type_parameter_e2e;
