//! The declaration-use checks of a committed member call: what the selected declaration permits at
//! this call site, after overload selection has chosen it.
//!
//! The spread check is kotlinc's `FirSignaturePolymorphicSpreadChecker`: the descriptor of a
//! signature-polymorphic call (`MethodHandle.invokeExact`, `VarHandle.get`) is built from the types
//! of its arguments at the call site, so an argument cannot be an array spread into it. Each `*` is
//! reported (`SPREAD_ON_SIGNATURE_POLYMORPHIC_CALL_ERROR`). The provider decides signature
//! polymorphism from the declaration (JLS 15.12.3), so the check holds for a `ct.sym` release view,
//! which drops the JDK's `@PolymorphicSignature` markers.

use super::*;

const SPREAD_ON_SIGNATURE_POLYMORPHIC_CALL: &str =
    "spread operator is prohibited for arguments to signature-polymorphic calls.";

impl Checker<'_> {
    /// Report what the selected member does not permit at `call`. Returns `false` when the member
    /// is inaccessible here, so the call has no result type.
    pub(super) fn check_selected_member_use(
        &mut self,
        call: ExprId,
        args: &[ExprId],
        selected: &crate::symbol_resolver::ResolvedMember,
    ) -> bool {
        if self.reject_inaccessible_selected_member(self.call_callee_name_span(call), selected) {
            return false;
        }
        self.check_data_class_copy_usage(self.call_callee_name_span(call), &selected.member);
        self.reject_spread_on_signature_polymorphic_call(args, &selected.member);
        self.reject_non_public_api_from_public_inline(
            call,
            Self::is_public_api_for_inline_access(
                selected.member.visibility,
                &selected.member.annotations,
            ),
            selected.member.inline,
        );
        true
    }

    /// Report every spread argument of a call that selected the signature-polymorphic `member`.
    fn reject_spread_on_signature_polymorphic_call(
        &mut self,
        args: &[ExprId],
        member: &crate::libraries::LibraryMember,
    ) {
        if !member.is_signature_polymorphic() {
            return;
        }
        for &argument in args {
            if let Some(star) = self.file.spread_star_span(argument) {
                self.diags.error_named(
                    star,
                    crate::diag::names::SPREAD_ON_SIGNATURE_POLYMORPHIC_CALL_ERROR,
                    SPREAD_ON_SIGNATURE_POLYMORPHIC_CALL,
                );
            }
        }
    }
}
