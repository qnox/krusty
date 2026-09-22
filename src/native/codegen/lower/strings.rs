//! What a program asks of a string beyond building one.
//!
//! Most of it is a table: `intrinsics::scalar_member` and `intrinsics::runtime_member` name the
//! runtime function for each member, because a string's members are all the runtime's and none of
//! them needs a decision the call site has to make. What lands here is the one shape those tables
//! cannot express — a PROPERTY read, which arrives as a checked node rather than as a call.
//!
//! Written in source, `String.length` is not one of them: the frontend names that as a
//! compiler-supplied operation, so it never reaches a dependency-member path. Two shapes do reach
//! here — a receiver typed `CharSequence`, because the property belongs to the interface, and a
//! read this backend SYNTHESIZED for a reference to the property, which is an accessor call like
//! any other.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// A read of TEXT's `length` — `CharSequence`'s, a builder's, or a string's.
    ///
    /// One runtime function answers all three, because one runtime reader (`kt_text_of`) reaches
    /// the bytes of each, and reaches text the PROGRAM declared through the descriptor. That is the
    /// same position the member table already takes for `CharSequence.get`, and for the same
    /// reason.
    pub(super) fn is_text_length(&self, target: crate::fir::ExternalPropertyId) -> bool {
        let Some(property) = self.file.provider.external_property(target) else {
            return false;
        };
        let Some(getter) = self.file.provider.external_callable(property.getter) else {
            return false;
        };
        super::super::super::intrinsics::is_text_length(getter.callable.owner, &property.name)
    }

    pub(super) fn text_length(&mut self, receiver: u32) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_string_length", &[any()], Ty::Int, &[value])
    }
}
