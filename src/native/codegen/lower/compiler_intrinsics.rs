//! Ordinary dependency-call forms of compiler-supplied operations.
//!
//! Source operators normally arrive as `IrIntrinsic`. A callable reference to the same selected
//! declaration arrives as an external call instead, retaining the provider's exact
//! `CompilerIntrinsic`. This boundary consumes that identity; it never reconstructs one from an
//! owner or callable spelling.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// Realize an exact provider intrinsic reached through an ordinary member call.
    ///
    /// Returns `None` only when the selected declaration has no operation handled here, allowing
    /// the caller to continue through ordinary dependency realization.
    pub(super) fn compiler_intrinsic_call(
        &mut self,
        intrinsic: Option<crate::libraries::CompilerIntrinsic>,
        receiver: Option<u32>,
        args: &[u32],
        ret: Ty,
    ) -> Option<Result<Option<Value>, Unsupported>> {
        match intrinsic {
            Some(crate::libraries::CompilerIntrinsic::StringPlus) => {
                let (Some(receiver), [argument]) = (receiver, args) else {
                    return Some(Err("a malformed `String.plus`".to_string()));
                };
                Some(self.string_plus(receiver, *argument, ret))
            }
            Some(crate::libraries::CompilerIntrinsic::BooleanNot) => {
                let (Some(receiver), []) = (receiver, args) else {
                    return Some(Err("a malformed `Boolean.not`".to_string()));
                };
                if self.type_of(receiver).map(Ty::non_null) != Some(Ty::Boolean) {
                    return Some(Err(
                        "a `Boolean.not` receiver with a non-Boolean type".to_string()
                    ));
                }
                Some(self.boolean_not(receiver, ret))
            }
            Some(crate::libraries::CompilerIntrinsic::StringGet) => {
                let (Some(receiver), [index]) = (receiver, args) else {
                    return Some(Err("a malformed `String.get`".to_string()));
                };
                Some(self.string_get(receiver, *index, ret))
            }
            _ => None,
        }
    }
}
