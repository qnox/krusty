//! Interface default members a class inherits without overriding them: the forwarders and
//! value-class `-impl` statics it declares for them, their names and signatures against kotlinc's
//! bytes, and the calls that reach them.

use super::common;

#[path = "../generic_default_forwarder_signature_e2e.rs"]
mod generic_default_forwarder_signature_e2e;
#[path = "../value_class_inherited_default_call_e2e.rs"]
mod value_class_inherited_default_call_e2e;
#[path = "../value_class_inherited_default_names_e2e.rs"]
mod value_class_inherited_default_names_e2e;
#[path = "../value_class_interface_defaults_e2e.rs"]
mod value_class_interface_defaults_e2e;
