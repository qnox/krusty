//! The `@Metadata` payload kotlinc writes on a lambda's own class: one `Function` message for the
//! lambda (`<anonymous>`, local visibility), its type parameters, receiver, value parameters and
//! result, read by reflection (`reflect()`) on the lambda object.

use std::collections::HashSet;

use crate::ir::type_reflection::type_parameters_named_by;
use crate::ir::IrTypeParameter;
use crate::metadata::protobuf::Pb;
use crate::metadata::type_encoder::{
    encode_metadata_type_parameter, encode_type, MetadataTypeParameter, StringTable,
    TypeParameterRef, TypeParameters,
};
use crate::types::{Ty, TypeName};

/// kotlinc's name for a lambda's function.
const ANONYMOUS: &str = "<anonymous>";

/// `Function.flags` of a lambda: final, `LOCAL` visibility (5), nothing else. kotlinc records a
/// suspend lambda's function without `IS_SUSPEND`.
const LAMBDA_FLAGS: u64 = 5 << 1;

/// A lambda's function as its class's metadata describes it. kotlinc records no context
/// parameters for a lambda.
pub(crate) struct LambdaFunction<'a> {
    pub receiver: Option<Ty>,
    /// Each value parameter's metadata name and type, in order.
    pub parameters: &'a [(&'a str, Ty)],
    pub result: Ty,
    /// The declarations of the type parameters the function type names, directly or through their
    /// bounds.
    pub type_parameters: &'a [IrTypeParameter],
    /// The file's classifiers whose class ids are local, and the enum entry bodies among them.
    pub local_classifiers: &'a HashSet<TypeName>,
    pub enum_entry_bodies: &'a HashSet<TypeName>,
    /// Checked declaration approximation for a non-denotable intersection in this signature.
    pub intersection_approximation: &'a dyn Fn(Ty) -> Option<Ty>,
}

/// `d1` (before its string packing) and `d2` for a lambda class.
///
/// kotlinc gives the lambda's function the type parameters its result, receiver and value
/// parameters name, numbered in that order of first use and referred to by name. A type parameter
/// only their bounds name stays the enclosing declaration's: it is numbered after them on first
/// use and referred to by id. The function's name and result are interned first, then its type
/// parameters, receiver and value parameters.
pub(crate) fn build(lambda: &LambdaFunction<'_>) -> (Vec<u8>, Vec<String>) {
    let mut strings = StringTable::with_local_classifiers(
        lambda.local_classifiers,
        lambda.enum_entry_bodies,
        Some(lambda.intersection_approximation),
    );
    let mut named = Vec::new();
    type_parameters_named_by(lambda.result, &mut named);
    if let Some(receiver) = lambda.receiver {
        type_parameters_named_by(receiver, &mut named);
    }
    for &(_, ty) in lambda.parameters {
        type_parameters_named_by(ty, &mut named);
    }
    let declaration = |name: &str| {
        lambda
            .type_parameters
            .iter()
            .find(|parameter| parameter.semantic_name == name)
            .expect("a lambda records the declaration of each type parameter it names")
    };
    let own: Vec<&IrTypeParameter> = named.iter().map(|name| declaration(name)).collect();
    let mut parameters = TypeParameters::classifier(
        own.len(),
        lambda
            .type_parameters
            .iter()
            .filter(|parameter| !named.contains(&parameter.semantic_name.as_str()))
            .map(|parameter| parameter.semantic_name.clone()),
    );
    for parameter in &own {
        parameters.insert(
            parameter.semantic_name.clone(),
            TypeParameterRef::Named(parameter.name.clone()),
        );
    }
    let encode = |strings: &mut StringTable<'_>, ty| {
        encode_type(strings, ty, &parameters)
            .unwrap_or_else(|error| panic!("invalid lambda metadata type: {error}"))
    };

    let mut function = Pb::new();
    // Interned in the order kotlinc's serializer visits them.
    function.field_varint(2, strings.local(ANONYMOUS) as u64); // Function.name = 2
    let result = encode(&mut strings, lambda.result);
    function.field_message(3, &result); // Function.return_type = 3
    for (id, parameter) in own.iter().enumerate() {
        let parameter = encode_metadata_type_parameter(
            &mut strings,
            id,
            &MetadataTypeParameter {
                name: parameter.name.clone(),
                reified: parameter.reified,
                variance: parameter.variance,
                upper_bounds: parameter.bounds.iter().map(|&(bound, _)| bound).collect(),
                upper_bound_spellings: Vec::new(),
            },
            &parameters,
        )
        .unwrap_or_else(|error| panic!("invalid lambda metadata type parameter: {error}"));
        function.repeated_message(4, &parameter); // Function.type_parameter = 4
    }
    if let Some(receiver) = lambda.receiver {
        let receiver = encode(&mut strings, receiver);
        function.field_message(5, &receiver); // Function.receiver_type = 5
    }
    for &(name, ty) in lambda.parameters {
        let mut parameter = Pb::new();
        parameter.field_varint(2, strings.local(name) as u64); // ValueParameter.name = 2
        let ty = encode(&mut strings, ty);
        parameter.field_message(3, &ty); // ValueParameter.type = 3
        function.repeated_message(6, &parameter); // Function.value_parameter = 6
    }
    function.field_varint(9, LAMBDA_FLAGS); // Function.flags = 9
    let types = strings.serialize_types();
    // The same framing as every other payload: a leading 0x00, the delimited string-table types,
    // then the message.
    let mut bytes = vec![0x00u8];
    let mut length = Pb::new();
    length.varint(types.as_bytes().len() as u64);
    bytes.extend_from_slice(&length.into_bytes());
    bytes.extend_from_slice(types.as_bytes());
    bytes.extend_from_slice(function.canonical().as_bytes());
    (bytes, strings.into_strings())
}
