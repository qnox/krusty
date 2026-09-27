//! The `@Metadata` payload kotlinc writes on a lambda's own class: one `Function` message for the
//! lambda (`<anonymous>`, local visibility), its receiver, value parameters and result, read by
//! reflection (`reflect()`) on the lambda object.

use crate::metadata::protobuf::Pb;
pub(crate) use crate::metadata::type_encoder::TypeEncodeError;
use crate::metadata::type_encoder::{encode_type, StringTable, TypeParameters};
use crate::types::Ty;

/// kotlinc's name for a lambda's function.
const ANONYMOUS: &str = "<anonymous>";

/// `Function.flags` of a lambda: final, `LOCAL` visibility (5), nothing else. kotlinc records a
/// suspend lambda's function without `IS_SUSPEND`.
const LAMBDA_FLAGS: u64 = 5 << 1;

/// A lambda's function as its class's metadata describes it.
pub(crate) struct LambdaFunction<'a> {
    pub receiver: Option<Ty>,
    /// Each value parameter's metadata name and type, in order.
    pub parameters: &'a [(&'a str, Ty)],
    pub result: Ty,
}

/// `d1` and `d2` for a lambda class. A type naming a type parameter is not written yet: kotlinc
/// copies the enclosing declaration's type parameters into the lambda's own table.
pub(crate) fn build(
    lambda: &LambdaFunction<'_>,
) -> Result<(Vec<u8>, Vec<String>), TypeEncodeError> {
    let mut strings = StringTable::default();
    let parameters = TypeParameters::new();
    let mut function = Pb::new();
    // Interned in field order, as kotlinc's serializer visits them.
    function.field_varint(2, strings.local(ANONYMOUS) as u64); // Function.name = 2
    let result = encode_type(&mut strings, lambda.result, &parameters)?;
    function.field_message(3, &result); // Function.return_type = 3
    if let Some(receiver) = lambda.receiver {
        let receiver = encode_type(&mut strings, receiver, &parameters)?;
        function.field_message(5, &receiver); // Function.receiver_type = 5
    }
    for &(name, ty) in lambda.parameters {
        let mut parameter = Pb::new();
        parameter.field_varint(2, strings.local(name) as u64); // ValueParameter.name = 2
        let ty = encode_type(&mut strings, ty, &parameters)?;
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
    Ok((bytes, strings.into_strings()))
}
