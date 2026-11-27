//! Declaration-publication handoff for one callable already selected by signature semantics.

use super::{DiagnosticId, OriginId, ResolvedTy, ResolvedTypeAbbreviation, SignatureScope};

/// Publication-only result projection over the exact selected call. The compact evaluator supplies
/// ordinary and recursively published operand types; implementations must consume selection facts
/// recorded at `origin`, never repeat lookup or overload selection.
pub trait SelectedResultPublicationSemantics {
    /// Typealias constructed by the call selected at `origin`, when one survives publication.
    fn call_result_abbreviation(&self, _origin: OriginId) -> Option<ResolvedTypeAbbreviation> {
        None
    }

    /// The abbreviation of the call's published result `published`: arguments the call
    /// inferred for the alias are those `published` binds.
    fn published_call_result_abbreviation(
        &self,
        origin: OriginId,
        _published: ResolvedTy,
    ) -> Option<ResolvedTypeAbbreviation> {
        self.call_result_abbreviation(origin)
    }

    /// The expectation under which the call selected at `origin` is checked again as a conditional
    /// branch beside `sibling`, when the call left a result-only formal open (`emptyMap()` beside
    /// `mapOf("O" to "K")`). `None` when the call fixed its result or no face of `sibling` binds it.
    fn open_result_expectation(
        &self,
        _scope: SignatureScope,
        _origin: OriginId,
        _sibling: ResolvedTy,
    ) -> Option<ResolvedTy> {
        None
    }

    fn call_result_publication(
        &self,
        _scope: SignatureScope,
        _origin: OriginId,
        _receiver: Option<(ResolvedTy, ResolvedTy)>,
        _arguments: &[(ResolvedTy, ResolvedTy)],
    ) -> Result<Option<ResolvedTy>, DiagnosticId> {
        Ok(None)
    }
}
