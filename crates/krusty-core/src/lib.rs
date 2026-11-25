//! Interned classifier identities. `TypeName` is a handle into one process-wide name tree; every
//! phase and every compiler plugin shares it, so it lives below both.

pub mod assertions;
pub mod context_parameters;
pub mod features;
pub mod fx_hash;
pub mod inline_parameter_modifier;
pub mod kotlin_version;
pub mod kt_string;
pub mod language_version;
pub mod name_tree;
mod qualified_name;
pub mod span;
mod spelling;
mod type_name;

pub use spelling::{
    existing_type_name, existing_type_name_child, type_name, type_name_from,
    type_name_from_multifile_facade,
};
pub use type_name::{
    existing_type_name_in, existing_type_name_nested_child, insert_type_name_in, same,
    type_name_child, type_name_nested_child, InternalName, TypeName, KOTLIN_ARRAY, KOTLIN_BOOLEAN,
    KOTLIN_BYTE, KOTLIN_CHAR, KOTLIN_DOUBLE, KOTLIN_FLOAT, KOTLIN_INT, KOTLIN_LONG, KOTLIN_NOTHING,
    KOTLIN_SHORT, KOTLIN_STRING, KOTLIN_UBYTE, KOTLIN_UINT, KOTLIN_ULONG, KOTLIN_UNIT,
    KOTLIN_USHORT,
};
