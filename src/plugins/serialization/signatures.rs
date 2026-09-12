//! Generic signatures owned by declarations synthesized by the serialization plugin.

use crate::ir::{IrGenericSig, IrTypeParameter};
use crate::types::Ty;

use super::GENERATED_SERIALIZER_FQ;

/// A generated serializer always implements a parameterized interface. Even a serializer with no
/// type parameters therefore needs a class signature: the descriptor erases
/// `GeneratedSerializer<Serialized>` to its raw interface identity.
pub(super) fn generated_serializer_signature(
    type_params: Vec<IrTypeParameter>,
    serialized: Ty,
) -> IrGenericSig {
    IrGenericSig {
        type_params,
        params: Vec::new(),
        ret: None,
        supers: vec![
            Ty::obj("kotlin/Any"),
            Ty::obj_args(GENERATED_SERIALIZER_FQ, &[serialized]),
        ],
    }
}
