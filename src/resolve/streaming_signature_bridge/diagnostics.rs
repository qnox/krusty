//! Diagnostic selection while pass-one signature expressions use the ordinary resolver.

use super::*;

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
