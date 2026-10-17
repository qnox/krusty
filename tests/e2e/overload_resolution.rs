//! Overload resolution and candidate specificity integration coverage.

use super::common;

#[path = "../default_argument_overload_tie_e2e.rs"]
mod default_argument_overload_tie_e2e;
#[path = "../overload_declaration_specificity_e2e.rs"]
mod overload_declaration_specificity_e2e;
#[path = "../overloaded_extension_e2e.rs"]
mod overloaded_extension_e2e;
#[path = "../overloaded_inferred_return_e2e.rs"]
mod overloaded_inferred_return_e2e;
#[path = "../overloaded_receiver_extension_expectation_e2e.rs"]
mod overloaded_receiver_extension_expectation_e2e;
