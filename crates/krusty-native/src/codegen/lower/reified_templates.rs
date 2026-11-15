//! A runtime type operation on a reified type parameter that no expansion substituted.
//!
//! Kotlin splices every call of a function with a reified parameter, so only that declaration's
//! own template still names the parameter: its body and the lambdas and anonymous objects it
//! declares. The template runs only if the declaration were called directly, which Kotlin/Native
//! answers with an `UnsupportedOperationException`. An operation that common IR proves belongs to
//! the template ([`IrFile::is_reified_template_operation`]) raises that failure in place; anywhere
//! else, a surviving parameter is a lowering defect the operation refuses.

use super::*;

impl BodyLowering<'_, '_, '_> {
    /// Whether `operation`, a runtime type operation on `ty`, executes Kotlin's direct-call
    /// failure: it names a reified parameter of the erased helper being emitted, or of the reified
    /// declaration whose own template this body is.
    pub(super) fn is_unresolved_reified_operation(&self, operation: u32, ty: Ty) -> bool {
        self.unresolved_reified
            .iter()
            .any(|parameter| ty.type_parameter_occurrence_bound(parameter).is_some())
            || self
                .file
                .ir
                .is_reified_template_operation(self.scope, operation, ty)
    }

    /// Raise Kotlin's failure for a reified operation reached outside an inline expansion.
    pub(super) fn reified_direct_call_failure(&mut self) -> Result<Option<Value>, Unsupported> {
        let message = self.string_literal(
            b"This function has a reified type parameter and thus can only be inlined at compilation time, not called directly.",
        )?;
        self.raise("kt_type_unsupported_operation_exception", message)
    }
}
