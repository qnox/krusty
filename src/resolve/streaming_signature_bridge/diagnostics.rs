//! Diagnostic selection while pass-one signature expressions use the ordinary resolver.

use super::*;

/// Publish one retained pass-one diagnostic through the ordinary source diagnostic sink.
pub(super) fn emit_production_signature_diagnostic(
    diagnostics: &mut crate::diag::DiagSink,
    diagnostic: &ProductionSignatureDiagnostic,
) {
    diagnostics.set_file(diagnostic.file);
    if let Some(identity) = diagnostic.identity {
        diagnostics.error_with_identity(diagnostic.span, identity, diagnostic.message.clone());
    } else {
        diagnostics.error(diagnostic.span, diagnostic.message.clone());
    }
}

/// Report every unconditional finding of `recorded`, in recording order. A finding that only
/// explains a declined declaration is reported with that declaration's failure instead.
pub(super) fn emit_unconditional(
    diagnostics: &mut crate::diag::DiagSink,
    recorded: &[ProductionSignatureDiagnostic],
) {
    for diagnostic in recorded
        .iter()
        .filter(|diagnostic| diagnostic.unconditional)
    {
        emit_production_signature_diagnostic(diagnostics, diagnostic);
    }
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn record_unresolved_reference(
        &self,
        declaration: crate::fir::DeclarationId,
        origin: crate::fir::OriginId,
        spelling: &str,
    ) -> crate::fir::DiagnosticId {
        self.record_source_diagnostic(
            declaration,
            origin,
            format!("unresolved reference '{spelling}'."),
        )
    }

    /// Report `UNSAFE_CALL` when the member exists on the receiver's non-null type; otherwise report
    /// `UNRESOLVED_REFERENCE` with the same receiver facts as body checking.
    pub(super) fn record_missing_member(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        receiver: Ty,
        spelling: &str,
    ) -> crate::fir::DiagnosticId {
        let non_null_member = if matches!(receiver, Ty::Nullable(_)) {
            match self.with_resolver(scope, |resolver| {
                Some(
                    resolver
                        .resolve_symbol(
                            crate::symbol_resolver::SymRecv::Value(receiver.non_null()),
                            spelling,
                            &[],
                            &[],
                        )
                        .is_some(),
                )
            }) {
                Ok(exists) => exists,
                Err(diagnostic) => return diagnostic,
            }
        } else {
            false
        };
        let location = match self.headers.signature_origins.get(origin) {
            Some(crate::fir::Origin::Source { file, span }) => Some((file, span)),
            _ => None,
        };
        match location {
            Some((file, span)) if non_null_member => self.record_source_diagnostic_at(
                scope.owner,
                file,
                Span::new(span.lo.saturating_sub(1), span.hi),
                format!(
                    "only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable \
                     receiver of type '{}'.",
                    receiver.source_name()
                ),
            ),
            _ => self.record_unresolved_member(scope, origin, receiver, spelling),
        }
    }

    pub(super) fn record_unresolved_member(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        receiver: Ty,
        spelling: &str,
    ) -> crate::fir::DiagnosticId {
        let hidden_deprecated = match self.with_resolver(scope, |resolver| {
            Some(resolver.receiver_has_hidden_deprecated_member(receiver, spelling))
        }) {
            Ok(hidden) => hidden,
            Err(diagnostic) => return diagnostic,
        };
        self.record_source_diagnostic(
            scope.owner,
            origin,
            crate::resolve::unresolved_member_message(spelling, receiver, hidden_deprecated),
        )
    }
}
