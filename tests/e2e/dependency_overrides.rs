//! Overrides of a Kotlin dependency's members: the signatures, mangled names, and forwarders a
//! consumer writes for them, against kotlinc's bytes.

use super::common;

#[path = "../dependency_override_signature_e2e.rs"]
mod dependency_override_signature_e2e;
#[path = "../dependency_suspend_default_forwarder_e2e.rs"]
mod dependency_suspend_default_forwarder_e2e;
#[path = "../dependency_value_class_override_e2e.rs"]
mod dependency_value_class_override_e2e;
