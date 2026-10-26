//! A value class's generated `toString`, `equals` and `hashCode` over its sole property, in the
//! shape of kotlinc's JVM backend.

use super::common;

#[path = "../value_class_array_to_string_e2e.rs"]
mod value_class_array_to_string_e2e;
#[path = "../value_class_equals_hash_e2e.rs"]
mod value_class_equals_hash_e2e;
#[path = "../value_class_nested_property_to_string_e2e.rs"]
mod value_class_nested_property_to_string_e2e;
