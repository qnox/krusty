//! Classifier-only traversal of the shared import scope tower.
//!
//! Callable collection deliberately drops records without callable facets. Classifier selection
//! must walk the same precedence levels without that filter so a constructor-only name remains
//! visible and aliases resolving to one identity collapse before ambiguity is decided.

use crate::symbol_source::SymbolSource;
use crate::types::TypeName;

use super::{classifier_candidates_at_scope_level, CandidateSelection, FunctionScopeRef};

pub(super) fn select(
    source: &dyn SymbolSource,
    scope: Option<FunctionScopeRef<'_>>,
    name: &str,
) -> CandidateSelection<TypeName> {
    let Some(scope) = scope else {
        return CandidateSelection::None;
    };
    if let FunctionScopeRef::Imports(imports) = scope {
        if let Some((owner, declared_name)) = imports.explicit_target(name) {
            if let Some(candidate) = source.symbols(owner, &declared_name).classifier_name {
                return CandidateSelection::Selected(candidate);
            }
        }
    }
    let levels: Vec<&[TypeName]> = match scope {
        FunctionScopeRef::Flat(packages) => vec![packages],
        FunctionScopeRef::Imports(imports) => imports.levels().iter().map(Vec::as_slice).collect(),
    };
    for level in levels {
        let candidates = classifier_candidates_at_scope_level(source, name, level);
        match candidates.as_slice() {
            [] => continue,
            [candidate] => return CandidateSelection::Selected(*candidate),
            _ => return CandidateSelection::Ambiguous,
        }
    }
    CandidateSelection::None
}
