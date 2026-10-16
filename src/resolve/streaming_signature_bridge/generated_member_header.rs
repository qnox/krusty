//! Final declaration-header facts for generated data-class `copy` members.
//!
//! Header extraction runs before class annotations are bound, so a generated data-class `copy`
//! carries provisional visibility. Signature collection owns its final visibility and exposes it
//! through the exact stable declaration identity. Every other generated member's inventory
//! visibility is already final and does not require a published callable signature.

use crate::fir::{DeclarationFlags, DeclarationKind, DeclarationStub};
use crate::types::Visibility;

use super::ProductionSignatureSemantics;

pub(super) fn visibility(
    semantics: &ProductionSignatureSemantics<'_>,
    stub: &DeclarationStub,
) -> Option<Visibility> {
    if stub.kind != DeclarationKind::Function || !stub.flags.has(DeclarationFlags::DATA_CLASS_COPY)
    {
        return Some(stub.visibility);
    }
    semantics
        .callable_signature(stub.id)
        .map(|signature| signature.visibility)
}
