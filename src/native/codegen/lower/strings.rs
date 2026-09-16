//! What a program asks of a string beyond building one.
//!
//! Most of it is a table: `intrinsics::scalar_member` and `intrinsics::runtime_member` name the
//! runtime function for each member, because a string's members are all the runtime's and none of
//! them needs a decision the call site has to make. What lands here is the one shape those tables
//! cannot express — a PROPERTY read, which arrives as a checked node rather than as a call.
//!
//! `String.length` is not one of them: the frontend names that as an intrinsic, so it never reaches
//! a dependency-member path at all. A receiver typed `CharSequence` does, because the property
//! belongs to the interface.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// `cs.length` where the receiver is typed `CharSequence`.
    ///
    /// Every `CharSequence` this target can produce is a string: `subSequence` answers one and
    /// nothing in the runtime makes another. That is the same position the member table already
    /// takes for `CharSequence.get`, and for the same reason.
    pub(super) fn is_char_sequence_length(&self, target: crate::fir::ExternalPropertyId) -> bool {
        let Some(property) = self.file.classpath.external_property(target) else {
            return false;
        };
        let Some(getter) = self.file.classpath.external_callable(property.getter) else {
            return false;
        };
        super::super::super::intrinsics::is_char_sequence_length(
            getter.callable.owner,
            &getter.callable.name,
        )
    }

    pub(super) fn char_sequence_length(
        &mut self,
        receiver: u32,
    ) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_string_length", &[any()], Ty::Int, &[value])
    }
}
