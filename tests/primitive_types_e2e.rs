//! Primitive values against reference types: nullable primitive `is` checks, primitive smart
//! casts, and a primitive reaching a reference supertype.

use super::common;

#[path = "is_nullable_primitive_e2e.rs"]
mod is_nullable_primitive_e2e;
#[path = "is_primitive_smartcast_e2e.rs"]
mod is_primitive_smartcast_e2e;
#[path = "primitive_reference_supertypes_e2e.rs"]
mod primitive_reference_supertypes_e2e;
