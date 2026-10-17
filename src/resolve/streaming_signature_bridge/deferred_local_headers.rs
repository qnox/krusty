//! Stable declarations whose entire semantic header is owned by active Pass 2 body checking.
//!
//! Their absent spelling marker gives Pass 2 exclusive ownership, so Pass 1 must not ask a
//! transitional provider for a seed that could select a same-spelled declaration outside the
//! body-local scope.

use crate::fir::{DeclarationFlags, DeclarationId, HeaderDeclarationKind, SignatureGraph};
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
