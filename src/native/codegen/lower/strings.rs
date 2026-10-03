//! What a program asks of a string beyond building one.
//!
//! Most of it is a table: `intrinsics::scalar_member` and `intrinsics::runtime_member` name the
//! runtime function for each member, because a string's members are all the runtime's. What lands
//! here is a property read, which arrives as a checked node rather than as a call, and exact
//! compiler intrinsics such as `String.get`, which must not be rediscovered from spellings.
//!
//! Written in source, `String.length` is not one of them: the frontend names that as a
//! compiler-supplied operation, so it never reaches a dependency-member path. Two shapes do reach
//! here — a receiver typed `CharSequence`, because the property belongs to the interface, and a
//! read this backend SYNTHESIZED for a reference to the property, which is an accessor call like
//! any other.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// The selected `String.plus(Any?): String` declaration.
    pub(super) fn string_plus(
        &mut self,
        receiver: u32,
        argument: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let left = self.reference(receiver)?;
        let right = self.reference(argument)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_string_plus", &[any(), any()], ret, &[left, right])
    }

    /// A read of TEXT's `length` — `CharSequence`'s, a builder's, or a string's.
    ///
    /// One runtime function answers all three, because one runtime reader (`kt_text_of`) reaches
    /// the bytes of each, and reaches text the PROGRAM declared through the descriptor. That is the
    /// same position the member table already takes for `CharSequence.get`, and for the same
    /// reason.
    pub(super) fn is_text_length(&self, target: crate::fir::ExternalPropertyId) -> bool {
        let Some(property) = self.file.callables.property(target) else {
            return false;
        };
        let Some(getter) = self.file.callables.callable(property.getter) else {
            return false;
        };
        getter.compiler_intrinsic == Some(crate::backend::BackendCompilerIntrinsic::StringLength)
            || super::super::super::intrinsics::is_non_string_text_length(
                getter.physical_owner,
                &property.name,
            )
    }

    pub(super) fn text_length(&mut self, receiver: u32) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(receiver)?;
        if self.terminated {
            return Ok(None);
        }
        self.runtime_call("kt_string_length", &[any()], Ty::Int, &[value])
    }

    /// The selected `String.get(Int): Char` declaration, carried by its compiler-intrinsic identity.
    pub(super) fn string_get(
        &mut self,
        receiver: u32,
        index: u32,
        ret: Ty,
    ) -> Result<Option<Value>, Unsupported> {
        let value = self.reference(receiver)?;
        let Some(index) = self.coerce(index, Ty::Int)? else {
            return Err("a `Unit` string index".to_string());
        };
        if self.terminated {
            return Ok(None);
        }
        let produced = self.runtime_call(
            "kt_string_get",
            &[any(), Ty::Int],
            Ty::Char,
            &[value, index],
        )?;
        let Some(produced) = produced else {
            return Ok(None);
        };
        self.convert(produced, Some(Ty::Char), ret)
    }
}
