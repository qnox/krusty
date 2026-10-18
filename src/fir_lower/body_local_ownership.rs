//! Incremental common-IR ownership for declarations inside executable local classifiers.

use crate::fir::{DeclarationId, ResolvedClassifierHeader, ResolvedModuleIndex};

/// Whether the nearest stable classifier owner has reached the semantic index.
pub(super) enum SemanticOwner<'a> {
    Complete(Option<&'a ResolvedClassifierHeader>),
    Deferred,
}

/// Finds the nearest stable classifier declaration before interpreting its semantic header.
///
/// A deferred local classifier may be nested in another complete classifier. Walking only
/// published headers would skip that nearest owner and incorrectly attach its members to the
/// outer class. The stable declaration inventory preserves the exact lexical owner while Pass 2
/// is still preparing its semantic header.
pub(super) fn semantic_owner(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
) -> SemanticOwner<'_> {
    if index.is_body_local_declaration(declaration) {
        let nearest = index
            .classifier_and_outer_class_declarations(declaration)
            .into_iter()
            .next();
        let Some(nearest) = nearest else {
            return SemanticOwner::Complete(None);
        };
        let complete = index.body_local_owner_complete(nearest)
            && index.body_local_owner_semantically_complete(nearest);
        return match (complete, index.classifier_header(nearest)) {
            (true, Some(owner)) => SemanticOwner::Complete(Some(owner)),
            (false, _) | (true, None) => SemanticOwner::Deferred,
        };
    }
    SemanticOwner::Complete(index.enclosing_classifier(declaration))
}
