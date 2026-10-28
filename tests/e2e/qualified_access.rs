//! Qualified-expression root, package-qualifier, and qualified-access cases grouped under the bounded e2e test target.

pub(super) use super::common;

#[path = "../backticked_dotted_package_qualifier_e2e.rs"]
mod backticked_dotted_package_qualifier_e2e;
#[path = "../package_qualified_type_no_import_e2e.rs"]
mod package_qualified_type_no_import_e2e;
#[path = "../qualified_call_receiver_lambda_e2e.rs"]
mod qualified_call_receiver_lambda_e2e;
#[path = "../qualified_name_e2e.rs"]
mod qualified_name_e2e;
#[path = "../qualifier_root_extension_property_e2e.rs"]
mod qualifier_root_extension_property_e2e;
#[path = "../root_package_cast_message_e2e.rs"]
mod root_package_cast_message_e2e;
