//! Declaration-time validity rules for `tailrec` members.

use crate::ast::ClassDecl;
use crate::diag::DiagSink;

/// Reject `tailrec` on a member that can still be overridden.
///
/// The modifier promises a loop, which is only sound when the self-call cannot dispatch somewhere
/// else. Overridability belongs to the member and its owner: a bare `override` remains open, but it
/// cannot be overridden further when its containing class is final.
pub(super) fn check_members(diags: &mut DiagSink, class: &ClassDecl) {
    if !(class.is_open() || class.is_abstract() || class.is_interface()) {
        return;
    }
    for method in &class.methods {
        if method.is_tailrec() && method.is_open() {
            let span = method.tailrec_span.unwrap_or(method.signature_span);
            diags.error(span, "tailrec is prohibited on open members.".to_string());
        }
    }
}
