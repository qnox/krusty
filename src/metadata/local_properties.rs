//! A local delegated property as Kotlin metadata records it: kotlinc lists the local delegated
//! properties of a class (JvmProtoBuf `classLocalVariable`, f102) or of a file facade
//! (`packageLocalVariable`, f102) in the order their `<v#N>` signatures number them, so reflection
//! can resolve a `KProperty` of one.

use crate::metadata::property_flags;
use crate::metadata::protobuf::Pb;
use crate::metadata::type_encoder::{
    encode_metadata_type_parameter, encode_type, MetadataTypeParameter, StringTable,
    TypeParameterRef, TypeParameters,
};
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
    /// Enclosing type parameters referenced by this local property's type, copied onto the local
    /// property record as kotlinc does. Semantic identity and source name remain separate.
    pub type_parameters: Vec<LocalPropertyTypeParameter>,
}

pub struct LocalPropertyTypeParameter {
    pub source_name: String,
    pub semantic_name: String,
    pub upper_bound: Ty,
}

/// The `Property` record: name (f2), return type (f3), and a local, delegated flag word (f11).
pub(crate) fn local_property_pb(
    st: &mut StringTable,
    property: &LocalPropertyMeta,
    type_parameters: &TypeParameters,
) -> Pb {
    let mut record = Pb::new();
    // A local property's copied parameters form an isolated declaration table: kotlinc numbers
    // them from zero and refers to them by source name, even when the identity came from an
    // enclosing class parameter. Keep unrelated enclosing parameters available for bounds.
    let mut property_type_parameters = type_parameters.clone();
    for parameter in &property.type_parameters {
        let reference = TypeParameterRef::Named(parameter.source_name.clone());
        property_type_parameters.insert(parameter.source_name.clone(), reference.clone());
        property_type_parameters.insert(parameter.semantic_name.clone(), reference);
    }
    record.field_varint(2, st.local(&property.name) as u64);
    let ty = encode_type(st, property.ty, &property_type_parameters)
        .unwrap_or_else(|error| panic!("invalid emitted metadata type: {error}"));
    record.field_message(3, &ty);
    for (ordinal, parameter) in property.type_parameters.iter().enumerate() {
        let implicit_bound = Ty::nullable(Ty::obj("kotlin/Any"));
        let encoded = encode_metadata_type_parameter(
            st,
            ordinal,
            &MetadataTypeParameter {
                name: parameter.source_name.clone(),
                reified: false,
                variance: crate::types::TypeVariance::Invariant,
                upper_bounds: (parameter.upper_bound != implicit_bound)
                    .then_some(parameter.upper_bound)
                    .into_iter()
                    .collect(),
                upper_bound_spellings: Vec::new(),
            },
            &property_type_parameters,
        )
        .unwrap_or_else(|error| panic!("invalid local-property type parameter: {error}"));
        record.repeated_message(4, &encoded);
    }
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
