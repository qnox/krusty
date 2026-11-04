//! Which declarations a call site can see: private, protected and internal members, classifiers
//! and constructors, from source and from the classpath, and the extensions that take a call an
//! invisible member cannot.

pub(super) use super::common;

#[path = "../classpath_protected_member_e2e.rs"]
mod classpath_protected_member_e2e;
#[path = "../classpath_protected_nested_access_e2e.rs"]
mod classpath_protected_nested_access_e2e;
#[path = "../file_private_extension_scope_e2e.rs"]
mod file_private_extension_scope_e2e;
#[path = "../hidden_member_extension_e2e.rs"]
mod hidden_member_extension_e2e;
#[path = "../internal_classpath_access_e2e.rs"]
mod internal_classpath_access_e2e;
#[path = "../internal_operator_member_e2e.rs"]
mod internal_operator_member_e2e;
#[path = "../private_classifier_access_e2e.rs"]
mod private_classifier_access_e2e;
#[path = "../private_constructor_access_e2e.rs"]
mod private_constructor_access_e2e;
