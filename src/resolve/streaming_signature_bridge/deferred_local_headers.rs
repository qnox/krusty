//! Stable declarations whose semantic header may require active Pass 2 body checking.
//!
//! This is a candidate set, not a completion decision. Pass 1 publishes representable function and
//! constructor contracts. Body-local property/accessor surfaces are owner-completed even when
//! explicitly typed, while unpublishable headers and the inseparable primary-vararg relation also
//! transfer atomically to Pass 2.

use crate::fir::{
    DeclarationFlags, DeclarationId, DeclarationKind, HeaderDeclarationKind, SignatureGraph,
};
use crate::fx_hash::HashSet;

#[cfg(test)]
mod tests;

pub(super) fn declarations(
    headers: &crate::fir::StreamedHeaderModule,
    graph: &SignatureGraph,
) -> HashSet<DeclarationId> {
    let explicit_classifier_parents = graph
        .explicit_classifier_parents()
        .map(|(declaration, _)| declaration)
        .collect::<HashSet<_>>();
    headers
        .stubs
        .iter()
        .filter(|stub| {
            let compact = headers.syntax.declaration(stub.id);
            let owns_signature_default = compact.is_some_and(|declaration| {
                let parameters = match declaration.kind {
                    HeaderDeclarationKind::Callable { parameters, .. }
                    | HeaderDeclarationKind::Constructor { parameters, .. } => Some(parameters),
                    HeaderDeclarationKind::Classifier { .. }
                    | HeaderDeclarationKind::Property { .. }
                    | HeaderDeclarationKind::TypeAlias { .. } => None,
                };
                parameters.is_some_and(|parameters| {
                    headers
                        .syntax
                        .parameters(parameters)
                        .iter()
                        .any(|parameter| parameter.flags.has_default())
                })
            });
            let owns_classifier_context_or_bound = compact.is_some_and(|declaration| {
                matches!(
                    declaration.kind,
                    HeaderDeclarationKind::Classifier {
                        context_parameters,
                        bounds,
                        ..
                    } if !headers.syntax.parameters(context_parameters).is_empty()
                        || !headers.syntax.bounds(bounds).is_empty()
                )
            });
            stub.flags.has(DeclarationFlags::LOCAL_CLASS)
                && !stub.flags.has(DeclarationFlags::INLINE)
                && !owns_signature_default
                && !owns_classifier_context_or_bound
                && graph.constraint(stub.id).is_none()
                && graph.explicit_signature_types(stub.id).is_none()
                && !explicit_classifier_parents.contains(&stub.id)
        })
        .map(|stub| stub.id)
        .collect()
}

/// Whether this declaration must remain owned by its active semantic classifier in Pass 2.
///
/// `candidates` identifies headers whose semantic seed itself is unpublishable. Explicitly typed
/// body-local properties/accessors do not belong to that set, but their lexical member surface is
/// still completed only when the active owner is entered. Functions keep their representable
/// Pass-1 contracts; constructor properties do too unless the primary-vararg relation is atomic.
pub(super) fn defers_owner_completed_signature(
    stub: &crate::fir::DeclarationStub,
    atomic_vararg_relation: bool,
    candidates: &HashSet<DeclarationId>,
) -> bool {
    let body_local = stub.flags.has(DeclarationFlags::LOCAL_CLASS);
    match stub.kind {
        DeclarationKind::Property => {
            body_local
                && (!stub.flags.has(DeclarationFlags::PROPERTY_PARAMETER) || atomic_vararg_relation)
        }
        DeclarationKind::Accessor => body_local,
        DeclarationKind::Constructor => candidates.contains(&stub.id) && atomic_vararg_relation,
        DeclarationKind::Function
        | DeclarationKind::Classifier
        | DeclarationKind::TypeAlias
        | DeclarationKind::Initializer
        | DeclarationKind::EnumEntry
        | DeclarationKind::Script => false,
    }
}

/// Whether an unpublishable executable seed may be completed by the active Pass-2 checker.
///
/// Constructors are not unconditionally late: a representable constructor is required while the
/// same local body resolves construction and super-construction calls. A candidate executable
/// whose semantic seed is still unresolved is different; publishing `Error` is impossible, so its
/// exact active declaration completes the header in Pass 2.
pub(super) fn may_defer_unpublishable_signature(
    declaration: DeclarationId,
    kind: DeclarationKind,
    candidates: &HashSet<DeclarationId>,
) -> bool {
    candidates.contains(&declaration)
        && matches!(
            kind,
            DeclarationKind::Function
                | DeclarationKind::Property
                | DeclarationKind::Constructor
                | DeclarationKind::Accessor
        )
}

/// A local declaration's compact semantic header may be complete while projection of one source
/// occurrence still needs the active lexical scope. That spelling is published by the checked
/// inline/default/body fragment. Non-local spelling failures remain Pass-1 failures.
pub(super) fn retain_non_local_spelling_failures(
    headers: &crate::fir::StreamedHeaderModule,
    failures: &mut Vec<DeclarationId>,
) {
    failures.retain(|declaration| {
        !headers
            .stubs
            .iter()
            .any(|stub| stub.id == *declaration && stub.flags.has(DeclarationFlags::LOCAL_CLASS))
    });
}

/// Remove the legacy symbol table's physical capture prefix from a local constructor seed. The
/// stable signature is a source ABI; checked FIR/backends own capture storage separately.
pub(super) fn retain_source_constructor_parameters(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: DeclarationId,
    parameters: &mut Vec<crate::types::Ty>,
) -> bool {
    let Some(syntax) = headers.syntax.declaration(declaration) else {
        return false;
    };
    let HeaderDeclarationKind::Constructor {
        context_parameters,
        parameters: source_parameters,
    } = syntax.kind
    else {
        return false;
    };
    let source_count = headers.syntax.parameters(context_parameters).len()
        + headers.syntax.parameters(source_parameters).len();
    let Some(capture_count) = parameters.len().checked_sub(source_count) else {
        return false;
    };
    parameters.drain(..capture_count);
    true
}

/// Nominal classifier headers belong to Pass 1 even when their executable local signatures need
/// an active lexical body. Their formals and applied layouts publish in the same pass.
pub(super) fn classifiers(
    headers: &crate::fir::StreamedHeaderModule,
) -> impl Iterator<Item = &crate::fir::DeclarationStub> {
    headers
        .stubs
        .iter()
        .filter(|stub| stub.kind == crate::fir::DeclarationKind::Classifier)
}
