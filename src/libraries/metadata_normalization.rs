//! Target-free normalization of decoded Kotlin metadata into the common library model.
//!
//! Every provider that reads Kotlin metadata, whatever archive it came from, converts a decoded
//! declaration here before any target representation is chosen. A target adapter may attach
//! physical realization facts afterwards; it must not convert the declaration a second way.

mod package_declarations;
mod parameter_identities;
mod type_parameter_identities;
mod type_signatures;

pub(crate) use package_declarations::{package_function, package_property, PropertyAccessorNames};
pub(crate) use parameter_identities::{
    function_parameter_identities, property_parameter_identities, FunctionParameterIdentities,
    PropertyParameterIdentities,
};
pub(crate) use type_parameter_identities::TypeParameterIdentities;
pub(crate) use type_signatures::{
    function_generic_sig, function_generic_sig_with_identities, only_input_type_formals,
    only_input_type_formals_with_identities, property_generic_sig_with_identities,
    reified_type_parameter_ordinals,
};
