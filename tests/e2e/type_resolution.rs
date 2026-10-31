//! Type-resolution conformance cases grouped under the bounded e2e test target.

pub(super) use super::common;

#[path = "../array_expression_type_validity_e2e.rs"]
mod array_expression_type_validity_e2e;

#[path = "../object_qualifier_type_arguments_e2e.rs"]
mod object_qualifier_type_arguments_e2e;
#[path = "../package_qualifier_forms_e2e.rs"]
mod package_qualifier_forms_e2e;
#[path = "../package_qualifier_type_arguments_e2e.rs"]
mod package_qualifier_type_arguments_e2e;
