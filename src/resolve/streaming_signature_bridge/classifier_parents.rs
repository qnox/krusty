//! Direct classifier parents published from compact Pass-1 syntax, with the superclass kept in its
//! written slot among the interfaces.

use std::collections::HashSet;

use super::{semantic_type_with_classifier_captures, ProductionSignatureSemantics};
use crate::resolve::ClassSig;
use crate::types::{Ty, TypeName};

/// Publish classifier inheritance from compact Pass-1 syntax. Every source-written edge is resolved
/// from its compact semantic type. The transitional collector contributes only language-defined
/// implicit parents, never a fallback spelling or type argument for a source edge.
pub(super) fn compact_classifier_parents(
    headers: &crate::fir::StreamedHeaderModule,
    semantics: &ProductionSignatureSemantics<'_>,
    declaration: crate::fir::DeclarationId,
    source: crate::fir::SourceFileId,
    classifier: &ClassSig,
    resolved_local: Option<&(Option<Ty>, Vec<Ty>)>,
    compact_cycle_edges: &HashSet<(crate::fir::DeclarationId, TypeName)>,
) -> Option<(Option<crate::fir::DeclaredSuperclass>, Vec<Ty>, Vec<Ty>)> {
    let header = headers.syntax.declaration(declaration)?;
    let crate::fir::HeaderDeclarationKind::Classifier {
        supertypes, base, ..
    } = header.kind
    else {
        return None;
    };
    let scope = crate::fir::SignatureScope {
        owner: declaration,
        source,
    };
    let explicit_base = resolved_local
        .and_then(|(base, _)| *base)
        .or_else(|| base.and_then(|syntax| semantics.resolve_compact_header_type(scope, syntax)));
    if base.is_some() && explicit_base.is_none() {
        crate::trace_compiler!(
            "signature",
            "classifier parent resolution declaration={declaration:?} failed explicit base resolved_local={resolved_local:?}",
        );
        return None;
    }
    let source_syntax = headers.syntax.type_operands(supertypes);
    if resolved_local.is_some_and(|(_, parents)| parents.len() != source_syntax.len()) {
        crate::trace_compiler!(
            "signature",
            "classifier parent resolution declaration={declaration:?} compact/source supertype count mismatch compact={} source={}",
            resolved_local.map_or(0, |(_, parents)| parents.len()),
            source_syntax.len(),
        );
        return None;
    }
    let resolved_source_supertypes = source_syntax
        .iter()
        .enumerate()
        .map(|(ordinal, syntax)| {
            resolved_local
                .and_then(|(_, parents)| parents.get(ordinal).copied())
                .or_else(|| semantics.resolve_compact_header_type(scope, *syntax))
        })
        .collect::<Option<Vec<_>>>()?;

    let parent_is_interface = |parent: Ty| {
        if matches!(parent.non_null(), Ty::Fun(_)) {
            return Some(true);
        }
        let owner = parent.non_null().kotlin_class_internal()?;
        semantics
            .table
            .class_by_type_name(owner)
            .map(ClassSig::is_interface)
            .or_else(|| {
                semantics
                    .table
                    .libraries
                    .classifier(owner)
                    .map(|classifier| classifier.is_interface())
            })
    };
    let source_superclass = if explicit_base.is_some() {
        None
    } else {
        resolved_source_supertypes
            .iter()
            .position(|parent| parent_is_interface(*parent) == Some(false))
    };
    if resolved_source_supertypes
        .iter()
        .enumerate()
        .any(|(ordinal, parent)| {
            parent_is_interface(*parent).is_none()
                || parent_is_interface(*parent) == Some(false) && Some(ordinal) != source_superclass
        })
    {
        crate::trace_compiler!(
            "signature",
            "classifier parent resolution declaration={declaration:?} has invalid source parents={resolved_source_supertypes:?} classifications={:?} selected_superclass={source_superclass:?}",
            resolved_source_supertypes
                .iter()
                .map(|parent| parent_is_interface(*parent))
                .collect::<Vec<_>>(),
        );
        return None;
    }

    // Signature collection has already validated the complete module hierarchy and removed only
    // edges that participate in a source cycle. Compact type resolution above recovers the applied
    // source shapes, but must not resurrect one of those rejected nominal edges merely because its
    // syntax still exists in the header inventory.
    let retained_parent = |parent: Ty| {
        if matches!(parent.non_null(), Ty::Fun(_)) {
            return true;
        }
        parent
            .non_null()
            .kotlin_class_internal()
            .is_some_and(|owner| {
                if resolved_local.is_some() {
                    // A body-local alias is visible only while its bounded Pass-1 unit is live. The
                    // compact graph has already expanded and resolved that edge; the transitional
                    // collector can retain only the unresolvable alias spelling and therefore cannot
                    // validate its identity. Reject semantic cycle edges using the completed compact
                    // graph, otherwise make its resolved parent authoritative.
                    return !compact_cycle_edges.contains(&(declaration, owner));
                }
                {
                    classifier.super_internal == Some(owner)
                        || classifier
                            .interfaces
                            .iter_ids()
                            .any(|parent| parent == owner)
                }
            })
    };
    let superclass_slot =
        crate::fir::superclass_slot(&headers.syntax, source_syntax, base, source_superclass)?;
    let source_interfaces = resolved_source_supertypes
        .iter()
        .copied()
        .enumerate()
        .filter(|(ordinal, parent)| Some(*ordinal) != source_superclass && retained_parent(*parent))
        .collect::<Vec<_>>();
    let written_before = source_interfaces
        .iter()
        .filter(|(ordinal, _)| *ordinal < superclass_slot)
        .count();
    let superclass = explicit_base
        .or_else(|| source_superclass.map(|ordinal| resolved_source_supertypes[ordinal]))
        .filter(|parent| retained_parent(*parent))
        .map(|ty| crate::fir::DeclaredSuperclass::after(ty, written_before))
        .or_else(|| {
            let owner = classifier.super_internal?;
            let implicit = semantic_type_with_classifier_captures(
                semantics.table,
                Ty::obj_args_name(owner, &classifier.super_type_args),
            );
            (!implicit.mentions_error()).then(|| crate::fir::DeclaredSuperclass::after(implicit, 0))
        });

    let mut interfaces = source_interfaces
        .into_iter()
        .map(|(_, parent)| parent)
        .collect::<Vec<_>>();
    // Add validated implicit language parents from ClassSig. Source interfaces already carry their
    // compact applied arguments above; compare by resolved identity rather than relying on an
    // ordinal, because cycle removal can shrink the validated source-interface list.
    for (ordinal, owner) in classifier.interfaces.iter_ids().enumerate() {
        if interfaces
            .iter()
            .any(|parent| parent.non_null().kotlin_class_internal() == Some(owner))
        {
            continue;
        }
        let implicit = semantic_type_with_classifier_captures(
            semantics.table,
            Ty::obj_args_name(
                owner,
                classifier
                    .interface_type_args
                    .get(ordinal)
                    .map(Vec::as_slice)
                    .unwrap_or_default(),
            ),
        );
        if implicit.mentions_error() {
            crate::trace_compiler!(
                "signature",
                "classifier parent resolution declaration={declaration:?} implicit interface {owner} is unpublishable as {implicit:?}",
            );
            return None;
        }
        interfaces.push(implicit);
    }
    Some((superclass, interfaces, resolved_source_supertypes))
}
