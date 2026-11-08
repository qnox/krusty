//! Member-extension call, property, and inference cases grouped under the bounded e2e test target.

pub(super) use super::common;

#[path = "../classpath_object_member_extension_import_e2e.rs"]
mod classpath_object_member_extension_import_e2e;
#[path = "../generic_member_extension_lambda_result_e2e.rs"]
mod generic_member_extension_lambda_result_e2e;
#[path = "../member_extension_default_args_e2e.rs"]
mod member_extension_default_args_e2e;
#[path = "../member_extension_default_forwarder_e2e.rs"]
mod member_extension_default_forwarder_e2e;
#[path = "../member_extension_function_e2e.rs"]
mod member_extension_function_e2e;
#[path = "../member_extension_in_lambda_e2e.rs"]
mod member_extension_in_lambda_e2e;
#[path = "../member_extension_lambda_arity_e2e.rs"]
mod member_extension_lambda_arity_e2e;
#[path = "../member_extension_lambda_inference_e2e.rs"]
mod member_extension_lambda_inference_e2e;
#[path = "../member_extension_nested_hof_e2e.rs"]
mod member_extension_nested_hof_e2e;
#[path = "../member_extension_property_e2e.rs"]
mod member_extension_property_e2e;
#[path = "../member_extension_property_override_e2e.rs"]
mod member_extension_property_override_e2e;
#[path = "../member_extension_receiver_join_e2e.rs"]
mod member_extension_receiver_join_e2e;
#[path = "../member_extension_receiver_supertype_e2e.rs"]
mod member_extension_receiver_supertype_e2e;
#[path = "../member_extension_recursive_bound_e2e.rs"]
mod member_extension_recursive_bound_e2e;
#[path = "../member_extension_reified_e2e.rs"]
mod member_extension_reified_e2e;
#[path = "../private_member_extension_accessor_e2e.rs"]
mod private_member_extension_accessor_e2e;
#[path = "../projected_member_extension_invoke_e2e.rs"]
mod projected_member_extension_invoke_e2e;
