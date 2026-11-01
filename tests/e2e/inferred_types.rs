//! Cases for types inferred from initializers, arguments, and lower bounds, grouped under the bounded e2e test target.

pub(super) use super::common;

#[path = "../inferred_alias_abbreviation_e2e.rs"]
mod inferred_alias_abbreviation_e2e;
#[path = "../inferred_common_supertype_e2e.rs"]
mod inferred_common_supertype_e2e;
#[path = "../inferred_computed_prop_e2e.rs"]
mod inferred_computed_prop_e2e;
#[path = "../inferred_constructor_argument_overload_e2e.rs"]
mod inferred_constructor_argument_overload_e2e;
#[path = "../inferred_property_type_args_e2e.rs"]
mod inferred_property_type_args_e2e;
