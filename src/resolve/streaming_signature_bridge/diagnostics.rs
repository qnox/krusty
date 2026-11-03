//! Diagnostic selection while pass-one signature expressions use the ordinary resolver.

use super::*;
use crate::types::TyKind;

/// One pass-one finding. A conditional finding explains a declaration whose publication fails and
/// is reported only with that failure; an unconditional one is reported on every exit.
#[derive(Clone)]
pub(super) struct ProductionSignatureDiagnostic {
    pub(super) declaration: crate::fir::DeclarationId,
    pub(super) file: u32,
    pub(super) span: Span,
    pub(super) message: String,
    pub(super) identity: Option<crate::diag::DiagnosticIdentity>,
    pub(super) unconditional: bool,
    /// The candidates an ambiguity names. A candidate whose result was still being inferred when
    /// the call failed is named by the result the solver published for it, so the message reads
    /// as the body check's does for the same call; `message` keeps the pending rendering.
    named_candidates: Option<NamedCandidates>,
}

#[derive(Clone)]
struct NamedCandidates {
    spelling: String,
    candidates: Box<[crate::libraries::FunctionInfo]>,
}

impl ProductionSignatureDiagnostic {
    pub(super) fn conditional(
        declaration: crate::fir::DeclarationId,
        file: u32,
        span: Span,
        message: String,
    ) -> Self {
        Self {
            declaration,
            file,
            span,
            message,
            identity: None,
            unconditional: false,
            named_candidates: None,
        }
    }

    pub(super) fn unconditional(
        declaration: crate::fir::DeclarationId,
        file: u32,
        span: Span,
        message: String,
        identity: crate::diag::DiagnosticIdentity,
    ) -> Self {
        Self {
            identity: Some(identity),
            unconditional: true,
            ..Self::conditional(declaration, file, span, message)
        }
    }

    /// The message as reported: an ambiguity names each candidate by its published result.
    fn reported_message(&self, published: &PublishedResults<'_>) -> String {
        let Some(named) = &self.named_candidates else {
            return self.message.clone();
        };
        let candidates = named
            .candidates
            .iter()
            .map(|candidate| with_published_result(candidate, published))
            .collect::<Vec<_>>();
        crate::resolve::Checker::ambiguous_candidates_message(&named.spelling, &candidates)
    }
}

/// The result the signature solver published for a declaration, once it has finished.
pub(super) type PublishedResults<'a> = dyn Fn(crate::fir::DeclarationId) -> Option<Ty> + 'a;

/// No declaration has a published result yet.
pub(super) fn nothing_published(_: crate::fir::DeclarationId) -> Option<Ty> {
    None
}

/// The results `index` has published.
pub(super) fn published_in(
    index: &crate::fir::ResolvedModuleIndex,
) -> impl Fn(crate::fir::DeclarationId) -> Option<Ty> + '_ {
    |declaration| {
        index
            .signature(declaration)
            .map(|signature| signature.result.get())
    }
}

/// `candidate` named by its declaration's published result when its own still mentions a pending
/// inference. A declaration whose signature failed keeps the pending result; that failure is
/// reported at the declaration itself.
fn with_published_result(
    candidate: &crate::libraries::FunctionInfo,
    published: &PublishedResults<'_>,
) -> crate::libraries::FunctionInfo {
    let mut candidate = candidate.clone();
    if !candidate.semantic_signature().ret.mentions_pending() {
        return candidate;
    }
    if let Some(result) = candidate.stable_declaration.and_then(published) {
        candidate.callable.ret = result;
        if let Some(generic) = candidate.generic_sig.as_mut() {
            generic.ret = result;
        }
    }
    candidate
}

/// Retained findings together with the results that render the candidates they name.
pub(super) struct RetainedFindings<'a> {
    pub(super) recorded: &'a [ProductionSignatureDiagnostic],
    pub(super) published: &'a PublishedResults<'a>,
}

/// Publish one retained pass-one diagnostic through the ordinary source diagnostic sink.
pub(super) fn emit_production_signature_diagnostic(
    diagnostics: &mut crate::diag::DiagSink,
    diagnostic: &ProductionSignatureDiagnostic,
    published: &PublishedResults<'_>,
) {
    diagnostics.set_file(diagnostic.file);
    let message = diagnostic.reported_message(published);
    if let Some(identity) = diagnostic.identity {
        diagnostics.error_with_identity(diagnostic.span, identity, message);
    } else {
        diagnostics.error(diagnostic.span, message);
    }
}

/// Report every unconditional finding of `recorded`, in recording order. A finding that only
/// explains a declined declaration is reported with that declaration's failure instead.
pub(super) fn emit_unconditional(
    diagnostics: &mut crate::diag::DiagSink,
    recorded: &[ProductionSignatureDiagnostic],
    published: &PublishedResults<'_>,
) {
    for diagnostic in recorded
        .iter()
        .filter(|diagnostic| diagnostic.unconditional)
    {
        emit_production_signature_diagnostic(diagnostics, diagnostic, published);
    }
}

/// Report retained findings in recording order for a complete set of failed declarations. A
/// pre-publication caller that already emitted unconditional findings excludes them; a batched
/// publication includes newly recorded unconditional findings in the same ordered pass.
pub(super) fn emit_failures(
    diagnostics: &mut crate::diag::DiagSink,
    recorded: &[ProductionSignatureDiagnostic],
    failed: &crate::fx_hash::HashSet<crate::fir::DeclarationId>,
    include_unconditional: bool,
    published: &PublishedResults<'_>,
) {
    for diagnostic in recorded.iter().filter(|diagnostic| {
        (include_unconditional && diagnostic.unconditional)
            || (!diagnostic.unconditional && failed.contains(&diagnostic.declaration))
    }) {
        emit_production_signature_diagnostic(diagnostics, diagnostic, published);
    }
}

/// Report findings produced after solver finalization when publication rejects `declaration`.
/// Conditional findings for unrelated declarations remain deferred; unconditional access errors
/// still surface on every publication exit.
pub(super) fn emit_publication_failure(
    diagnostics: &mut crate::diag::DiagSink,
    recorded: &[ProductionSignatureDiagnostic],
    declaration: crate::fir::DeclarationId,
    published: &PublishedResults<'_>,
) {
    for diagnostic in recorded
        .iter()
        .filter(|diagnostic| diagnostic.unconditional || diagnostic.declaration == declaration)
    {
        emit_production_signature_diagnostic(diagnostics, diagnostic, published);
    }
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn record_source_diagnostic(
        &self,
        declaration: crate::fir::DeclarationId,
        origin: crate::fir::OriginId,
        message: String,
    ) -> crate::fir::DiagnosticId {
        self.record_source_finding(declaration, origin, message, None)
    }

    /// kotlinc's ambiguity for a call whose most specific candidates tie, naming each candidate by
    /// its resolved signature. The body check reports the same call with the same text, so the
    /// two phases publish one diagnostic.
    pub(super) fn record_ambiguous_call(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        spelling: &str,
        candidates: &[crate::libraries::FunctionInfo],
    ) -> crate::fir::DiagnosticId {
        let message = crate::resolve::Checker::ambiguous_candidates_message(spelling, candidates);
        let named = NamedCandidates {
            spelling: spelling.to_owned(),
            candidates: candidates.into(),
        };
        self.record_source_finding(scope.owner, origin, message, Some(named))
    }

    fn record_source_finding(
        &self,
        declaration: crate::fir::DeclarationId,
        origin: crate::fir::OriginId,
        message: String,
        named_candidates: Option<NamedCandidates>,
    ) -> crate::fir::DiagnosticId {
        let fallback = self
            .headers
            .stub(declaration)
            .map(|stub| (stub.source.raw(), stub.range));
        let location = match self.headers.signature_origins.get(origin) {
            Some(crate::fir::Origin::Source { file, span }) => Some((file.raw(), span)),
            Some(crate::fir::Origin::Synthetic { cause, .. }) => {
                match self.headers.signature_origins.get(cause) {
                    Some(crate::fir::Origin::Source { file, span }) => Some((file.raw(), span)),
                    Some(crate::fir::Origin::Synthetic { .. }) | None => None,
                }
            }
            None => None,
        }
        .or(fallback);
        let Some((file, span)) = location else {
            return Self::failure();
        };
        let mut diagnostics = self.diagnostics.borrow_mut();
        if let Some(index) = diagnostics.iter().position(|diagnostic| {
            diagnostic.file == file && diagnostic.span == span && diagnostic.message == message
        }) {
            return crate::fir::DiagnosticId::from_raw(index as u32 + 1);
        }
        diagnostics.push(ProductionSignatureDiagnostic {
            named_candidates,
            ..ProductionSignatureDiagnostic::conditional(declaration, file, span, message)
        });
        crate::fir::DiagnosticId::from_raw(diagnostics.len() as u32)
    }

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
        let non_null_member = if matches!(receiver.kind(), TyKind::Nullable(_)) {
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
