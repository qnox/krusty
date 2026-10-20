//! Continuation policy for declaration-level failures after signature-solver finalization.
//!
//! A body-local header may decline with the internal sentinel because its lexical scope belongs to
//! Pass 2. A real recorded source diagnostic is never deferral: it remains a Pass-1 publication
//! failure and is emitted for that exact declaration.

use super::ProductionSignatureDiagnostic;
use crate::fir::{DeclarationId, DiagnosticId};
use crate::fx_hash::HashSet;

pub(super) fn retain_or_report(
    diagnostics: &mut crate::diag::DiagSink,
    recorded: &[ProductionSignatureDiagnostic],
    declaration: DeclarationId,
    diagnostic: Option<DiagnosticId>,
    candidates: &HashSet<DeclarationId>,
    deferred: &mut HashSet<DeclarationId>,
    failed: &mut Vec<DeclarationId>,
) {
    let internal_decline = diagnostic.is_none_or(|diagnostic| diagnostic.raw() == 0);
    if candidates.contains(&declaration) && internal_decline {
        deferred.insert(declaration);
        return;
    }
    super::diagnostics::emit_publication_failure(diagnostics, recorded, declaration);
    failed.push(declaration);
}
