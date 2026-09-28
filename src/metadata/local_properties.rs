//! A local delegated property as Kotlin metadata records it: kotlinc lists the local delegated
//! properties of a class (JvmProtoBuf `classLocalVariable`, f102) or of a file facade
//! (`packageLocalVariable`, f102) in the order their `<v#N>` signatures number them, so reflection
//! can resolve a `KProperty` of one.

use crate::metadata::property_flags;
use crate::metadata::protobuf::Pb;
use crate::metadata::type_encoder::{encode_type, StringTable, TypeParameters};
use crate::types::Ty;

/// JvmProtoBuf `classLocalVariable` / `packageLocalVariable`.
pub(crate) const LOCAL_VARIABLE_FIELD: u32 = 102;

/// `local` in the visibility bits (1-3) of a property flag word.
const LOCAL_VISIBILITY: u64 = 5 << 1;

/// One local delegated property: its name, its type, and whether it is a `var`.
pub struct LocalPropertyMeta {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
}

/// The `Property` record: name (f2), return type (f3), and a local, delegated flag word (f11).
pub(crate) fn local_property_pb(
    st: &mut StringTable,
    property: &LocalPropertyMeta,
    type_parameters: &TypeParameters,
) -> Pb {
    let mut record = Pb::new();
    record.field_varint(2, st.local(&property.name) as u64);
    let ty = encode_type(st, property.ty, type_parameters)
        .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"));
    record.field_message(3, &ty);
    let mutable = match property.mutable {
        true => property_flags::IS_VAR | property_flags::HAS_SETTER,
        false => 0,
    };
    let flags = (property_flags::DEFAULT & !property_flags::VISIBILITY_MASK)
        | LOCAL_VISIBILITY
        | property_flags::IS_DELEGATED
        | mutable;
    record.field_varint(11, flags);
    record
}
