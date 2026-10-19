//! Stable declarations whose semantic header may require active Pass 2 body checking.
//!
//! This is a candidate set, not a completion decision. Pass 1 publishes every representable
//! callable and property contract. Only an unpublishable header, including the inseparable
//! primary-vararg constructor/property relation, transfers atomically to Pass 2.

use crate::fir::{
    DeclarationFlags, DeclarationId, DeclarationKind, HeaderDeclarationKind, SignatureGraph,
};
use crate::fx_hash::{HashMap, HashSet};

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

/// Whether the declaration's semantic header must remain atomic with active Pass-2 type binding.
///
/// A function's exact, pending-free source ABI may publish independently from its spelling
/// sidecar. A parser-recorded alias projection or declaration-owned generic scope keeps that
/// function atomic with the lexical binding that selected it. Properties and accessors retain
/// their aggregate lifecycle: their property shape and accessor surface publish together with the
/// active owner unless a retained Pass-1 body explicitly requires them.
pub(super) fn requires_active_type_binding(
    headers: &crate::fir::StreamedHeaderModule,
    stub: &crate::fir::DeclarationStub,
) -> bool {
    let Some(declaration) = headers.syntax.declaration(stub.id) else {
        return false;
    };
    match (stub.kind, declaration.kind) {
        (
            DeclarationKind::Function,
            HeaderDeclarationKind::Callable {
                receiver,
                parameters,
                result,
                type_parameters,
                bounds,
                ..
            },
        ) => {
            !headers.syntax.type_parameters(type_parameters).is_empty()
                || receiver
                    .into_iter()
                    .chain(
                        headers
                            .syntax
                            .parameters(parameters)
                            .iter()
                            .map(|parameter| parameter.ty),
                    )
                    .chain(match result {
                        crate::fir::HeaderResultType::Explicit(result) => Some(result),
                        crate::fir::HeaderResultType::ImplicitUnit
                        | crate::fir::HeaderResultType::Inferred => None,
                    })
                    .chain(headers.syntax.bounds(bounds).iter().map(|bound| bound.ty))
                    .any(|ty| headers.syntax.has_source_spelling_projection(ty))
        }
        (
            DeclarationKind::Accessor,
            HeaderDeclarationKind::Callable {
                receiver,
                parameters,
                result,
                bounds,
                ..
            },
        ) => {
            receiver.is_some()
                || !headers.syntax.parameters(parameters).is_empty()
                || matches!(result, crate::fir::HeaderResultType::Explicit(_))
                || !headers.syntax.bounds(bounds).is_empty()
        }
        (
            DeclarationKind::Property,
            HeaderDeclarationKind::Property {
                receiver,
                context_parameters,
                declared_type,
                getter_type,
                backing_field_type,
                bounds,
                ..
            },
        ) if !stub.flags.has(DeclarationFlags::PROPERTY_PARAMETER) => {
            receiver.is_some()
                || !headers.syntax.parameters(context_parameters).is_empty()
                || declared_type.is_some()
                || getter_type.is_some()
                || backing_field_type.is_some()
                || !headers.syntax.bounds(bounds).is_empty()
        }
        // Constructors and constructor properties can be required by calls in the same streamed
        // body. Classifier parents have their own explicit graph, and type aliases are always
        // module-signature work. Those mandatory Pass-1 headers use the explicit deferred-
        // occurrence lifecycle when compact binding is unavailable.
        _ => false,
    }
}

/// Whether a property's exact stable owner chain reaches an inline body retained in Pass 1.
///
/// Semantic declaration ownership is authoritative. Parser-hoisted local classifiers use their
/// stable lexical root only when no owner edge exists. The retained body needs the property
/// contract before accessor publication; ordinary body-local properties remain atomic with Pass 2.
pub(super) fn retained_inline_property(
    headers: &crate::fir::StreamedHeaderModule,
    stub: &crate::fir::DeclarationStub,
) -> bool {
    if stub.kind != DeclarationKind::Property {
        return false;
    }
    let mut declaration = Some(stub.id);
    let mut seen = HashSet::default();
    while let Some(candidate) = declaration {
        if !seen.insert(candidate) {
            return false;
        }
        if headers
            .stub(candidate)
            .is_some_and(|owner| owner.flags.has(DeclarationFlags::INLINE))
        {
            return true;
        }
        declaration = headers
            .declarations
            .anchor(candidate)
            .and_then(|anchor| anchor.owner)
            .or_else(|| headers.local_classifier_lexical_root(candidate));
    }
    false
}

/// Local declarations whose mandatory semantic header has no compact graph node owning one of
/// its declaration-metadata type roles. This is decided before solving: absence from this set is
/// never inferred later from a failed spelling projection.
fn written_spelling_roles(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::HeaderDeclaration,
) -> Vec<crate::fir::HeaderTypeId> {
    let mut roles = Vec::new();
    match declaration.kind {
        HeaderDeclarationKind::Callable {
            receiver,
            parameters,
            result,
            ..
        } => {
            roles.extend(receiver);
            roles.extend(
                headers
                    .syntax
                    .parameters(parameters)
                    .iter()
                    .map(|parameter| parameter.ty),
            );
            if let crate::fir::HeaderResultType::Explicit(result) = result {
                roles.push(result);
            }
        }
        HeaderDeclarationKind::Property {
            receiver,
            context_parameters,
            declared_type,
            getter_type,
            ..
        } => {
            roles.extend(receiver);
            roles.extend(
                headers
                    .syntax
                    .parameters(context_parameters)
                    .iter()
                    .map(|parameter| parameter.ty),
            );
            roles.extend(declared_type);
            roles.extend(getter_type);
        }
        HeaderDeclarationKind::Constructor { parameters, .. } => roles.extend(
            headers
                .syntax
                .parameters(parameters)
                .iter()
                .map(|parameter| parameter.ty),
        ),
        HeaderDeclarationKind::Classifier { .. } | HeaderDeclarationKind::TypeAlias { .. } => {}
    }
    roles
        .into_iter()
        .map(|role| headers.syntax.written_type_id(role))
        .collect()
}

fn spelling_roles_matching(
    headers: &crate::fir::StreamedHeaderModule,
    mut matches: impl FnMut(crate::fir::HeaderTypeId) -> bool,
) -> HashMap<DeclarationId, HashSet<crate::fir::HeaderTypeId>> {
    let mut selected = HashMap::default();
    for stub in headers
        .stubs
        .iter()
        .filter(|stub| stub.flags.has(DeclarationFlags::LOCAL_CLASS))
    {
        let Some(declaration) = headers.syntax.declaration(stub.id) else {
            continue;
        };
        let roles = written_spelling_roles(headers, declaration)
            .into_iter()
            .filter(|role| matches(*role))
            .collect::<HashSet<_>>();
        if !roles.is_empty() {
            selected.insert(stub.id, roles);
        }
    }
    selected
}

pub(super) fn unowned_spelling_roles(
    headers: &crate::fir::StreamedHeaderModule,
    graph: &SignatureGraph,
) -> HashMap<DeclarationId, HashSet<crate::fir::HeaderTypeId>> {
    spelling_roles_matching(headers, |role| !graph.declaration_type_site_is_bound(role))
}

pub(super) fn unselected_spelling_roles(
    headers: &crate::fir::StreamedHeaderModule,
    selected: &super::selected_type_facts::SelectedTypeFacts,
) -> HashMap<DeclarationId, HashSet<crate::fir::HeaderTypeId>> {
    spelling_roles_matching(headers, |role| {
        !selected.has_complete_declaration_spelling(&headers.syntax, role)
    })
}

/// Whether this declaration belongs to the atomic primary-vararg constructor/property relation.
///
/// A representable property contract is stable Pass-1 input even when its owner is body-local.
/// Withholding that contract until Pass 2 would make member checking depend on whether a later body
/// happens to demand it. Only an unpublishable atomic constructor/property pair moves together.
pub(super) fn defers_atomic_vararg_signature(
    stub: &crate::fir::DeclarationStub,
    atomic_vararg_relation: bool,
    candidates: &HashSet<DeclarationId>,
) -> bool {
    candidates.contains(&stub.id)
        && atomic_vararg_relation
        && matches!(
            stub.kind,
            DeclarationKind::Constructor | DeclarationKind::Property
        )
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

/// Close exact local declaration groups whose complete semantic surface was published in Pass 1.
/// Such a classifier does not re-enter Pass 2 signature publication merely to acquire a lifecycle
/// marker.
pub(super) fn publish_pass_one_owner_completions(
    headers: &crate::fir::StreamedHeaderModule,
    index: &mut crate::fir::ResolvedModuleIndex,
) {
    let declarations = headers
        .stubs
        .iter()
        .filter(|stub| {
            stub.kind == DeclarationKind::Classifier
                && stub.flags.has(DeclarationFlags::LOCAL_CLASS)
                && index.body_local_owner_semantically_complete(stub.id)
        })
        .map(|stub| stub.id)
        .collect::<Vec<_>>();
    index.publish_body_local_owner_completions(declarations);
}
