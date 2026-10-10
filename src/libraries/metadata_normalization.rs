//! Target-free normalization of decoded Kotlin metadata into the common library model.
//!
//! Every provider that reads Kotlin metadata, whatever archive it came from, converts a decoded
//! declaration here before any target representation is chosen. A target adapter may attach
//! physical realization facts afterwards; it must not convert the declaration a second way.

mod callable_declarations;
mod classifier_declarations;
mod parameter_identities;
mod type_alias_declarations;
mod type_parameter_identities;
mod type_signatures;

pub(crate) use callable_declarations::{
    declared_function, declared_property, CallablePlacement, PropertyAccessorNames,
};
pub(crate) use classifier_declarations::{
    classifier_shape, constructor_parameter_list, declared_constructor, declared_retention,
    declared_targets, enum_entries_getter, enum_value_of, enum_values, member_record,
    settle_no_arg_construction, ClassTypeParameters,
};
pub(crate) use parameter_identities::{
    associated_function_parameter_identities, associated_property_parameter_identities,
    constructor_parameter_identities, function_parameter_identities, property_parameter_identities,
    FunctionParameterIdentities, PropertyParameterIdentities,
};
pub(crate) use type_alias_declarations::declared_type_alias;
pub(crate) use type_parameter_identities::TypeParameterIdentities;
pub(crate) use type_signatures::{
    function_generic_sig, only_input_type_formals, reified_type_parameter_ordinals, EnclosingBounds,
};
