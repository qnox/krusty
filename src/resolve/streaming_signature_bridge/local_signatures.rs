//! Active-file publication of checked local declaration signatures and captures.

use super::*;
use crate::resolve::TypeInfo;

/// Same-parse test adapter for capture storage discovered by the legacy retained-file fixture.
/// Production ordinary bodies publish no capture declaration; their checked FIR carries field
/// coordinates directly.
#[cfg(test)]
pub(crate) fn publish_discovered_local_capture_declarations(
    file: &File,
    source: crate::fir::SourceFileId,
    table: &mut SymbolTable,
    index: &mut crate::fir::ResolvedModuleIndex,
) {
    use crate::fir::{
        DeclarationAnchor, DeclarationFlags, DeclarationKind, ResolvedDeclarationHeader,
    };

    let captures = table
        .anonymous_object_captures
        .iter()
        .filter(|((file, _), _)| *file == source.raw())
        .map(|(&(file, class), captures)| ((file, class), captures.clone()))
        .collect::<Vec<_>>();
    for ((file_index, transient), captures) in captures {
        let Some(Decl::Class(class_decl)) = file.decl_arena.get(transient.0 as usize) else {
            continue;
        };
        let Some(owner) =
            index.declaration_at(source, class_decl.span, DeclarationKind::Classifier)
        else {
            continue;
        };
        let Some(classifier) = table
            .anonymous_object_types
            .get(&(file_index, transient))
            .copied()
        else {
            continue;
        };
        for (ordinal, capture) in captures.iter().enumerate() {
            if capture.storage_ty.is_some() {
                continue;
            }
            let sibling = u32::MAX
                .checked_sub(u32::try_from(ordinal).expect("too many anonymous-object captures"))
                .expect("too many anonymous-object captures");
            let declaration = index.intern_checked_local_declaration(
                DeclarationAnchor {
                    source,
                    range: class_decl.span,
                    owner: Some(owner),
                    kind: DeclarationKind::Property,
                    sibling,
                },
                ResolvedDeclarationHeader {
                    kind: DeclarationKind::Property,
                    owner: Some(owner),
                    name: None,
                    visibility: crate::types::Visibility::Private,
                    flags: DeclarationFlags::default()
                        .with(DeclarationFlags::LOCAL_CLASS, true)
                        .with(DeclarationFlags::COMPILER_GENERATED, true)
                        .with(DeclarationFlags::FINAL, true),
                    initialization_order: None,
                },
                &capture.name,
            );
            if let Some(property) = table
                .class_by_type_name_mut(classifier)
                .and_then(|class| class.declared_props.get_mut(&capture.name))
            {
                debug_assert!(!property.source_visible);
                property.stable_declaration = Some(declaration);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn publish_checked_local_signatures_in_active_root(
    file: &File,
    active: &crate::fir::ActiveSourceDeclarations,
    source: crate::fir::SourceFileId,
    table: &SymbolTable,
    info: &super::super::TypeInfo,
    index: &mut crate::fir::ResolvedModuleIndex,
    selected_root: crate::fir::DeclarationId,
    selected_bodies: &std::collections::HashSet<crate::fir::DeclarationId>,
) -> Result<(), Vec<crate::fir::DeclarationId>> {
    publish_checked_local_signatures_selected(
        file,
        source,
        Some(table),
        table.libraries.as_ref(),
        info,
        index,
        None,
        Some(active),
        Some((active, selected_root, selected_bodies)),
        None,
    )
}

pub(crate) fn publish_checked_local_signatures_in_pass_two_root(
    file: &File,
    active: &crate::fir::ActiveSourceDeclarations,
    source: crate::fir::SourceFileId,
    platform: &dyn crate::libraries::SemanticPlatform,
    info: &super::super::TypeInfo,
    index: &mut crate::fir::ResolvedModuleIndex,
    selected_root: crate::fir::DeclarationId,
    selected_bodies: &std::collections::HashSet<crate::fir::DeclarationId>,
) -> Result<(), Vec<crate::fir::DeclarationId>> {
    publish_checked_local_signatures_selected(
        file,
        source,
        None,
        platform,
        info,
        index,
        None,
        Some(active),
        Some((active, selected_root, selected_bodies)),
        None,
    )
}

#[cfg(test)]
pub(crate) fn publish_checked_local_signatures(
    file: &File,
    source: crate::fir::SourceFileId,
    table: &SymbolTable,
    info: &super::super::TypeInfo,
    index: &mut crate::fir::ResolvedModuleIndex,
) -> Result<(), Vec<crate::fir::DeclarationId>> {
    let active = crate::fir::ActiveSourceDeclarations::bind_complete_source(file, source, index)
        .expect("focused local-signature publication must bind the live parser arena");
    publish_checked_local_signatures_selected(
        file,
        source,
        Some(table),
        table.libraries.as_ref(),
        info,
        index,
        None,
        Some(&active),
        None,
        None,
    )
}

pub(crate) fn publish_checked_inline_local_signatures(
    file: &File,
    source: crate::fir::SourceFileId,
    table: &SymbolTable,
    info: &super::super::TypeInfo,
    index: &mut crate::fir::ResolvedModuleIndex,
    inline_owners: &[crate::fir::DeclarationId],
    active: Option<&crate::fir::ActiveSourceDeclarations>,
) -> Result<(), Vec<crate::fir::DeclarationId>> {
    publish_checked_local_signatures_selected(
        file,
        source,
        Some(table),
        table.libraries.as_ref(),
        info,
        index,
        Some(inline_owners),
        active,
        None,
        None,
    )
}

/// Publish local declarations that the bounded signature-default check actually entered.
///
/// A default owned by a non-local callable may itself contain a local classifier. Selecting only
/// ancestors of the provider misses that classifier entirely; selecting every local declaration
/// in the retained source would publish ordinary bodies that were never checked. The checker's
/// lexical statement path records the exact transient classifiers it entered, including classes
/// with no captures, and this bridge expands only those stable classifier subtrees.
pub(crate) fn publish_checked_default_local_signatures(
    file: &File,
    active: &crate::fir::ActiveSourceDeclarations,
    source: crate::fir::SourceFileId,
    table: &SymbolTable,
    info: &super::super::TypeInfo,
    index: &mut crate::fir::ResolvedModuleIndex,
    providers: &[crate::fir::DeclarationId],
) -> Result<(), Vec<crate::fir::DeclarationId>> {
    // A default belonging to a local callable needs that callable and its classifier ancestors,
    // but not every ordinary member body of the classifier. Those bodies remain Pass-2 work. A
    // local classifier declared *inside* the default is different: its complete checked subtree is
    // executable payload of the retained default fragment and is selected below from the exact
    // declarations the checker entered while its default-expression depth was nonzero.
    let mut exact = providers.to_vec();
    for provider in providers {
        let mut owner = index
            .declaration_header(*provider)
            .and_then(|header| header.owner)
            .or_else(|| {
                index
                    .declaration_anchor(*provider)
                    .and_then(|anchor| anchor.owner)
            });
        while let Some(declaration) = owner {
            let Some(header) = index.declaration_header(declaration) else {
                break;
            };
            if header.kind == crate::fir::DeclarationKind::Classifier
                && header.flags.has(crate::fir::DeclarationFlags::LOCAL_CLASS)
            {
                exact.push(declaration);
            }
            owner = header.owner.or_else(|| {
                index
                    .declaration_anchor(declaration)
                    .and_then(|anchor| anchor.owner)
            });
        }
    }
    exact.sort_unstable();
    exact.dedup();

    let mut owners = info
        .checked_local_class_declarations
        .iter()
        .filter_map(|declaration| match file.decl(*declaration) {
            Decl::Class(_) => active.canonical_classifier_declaration(*declaration, index),
            Decl::Fun(_) | Decl::Property(_) => None,
        })
        .collect::<Vec<_>>();
    owners.sort_unstable();
    owners.dedup();
    if owners.is_empty() && exact.is_empty() {
        return Ok(());
    }
    publish_checked_local_signatures_selected(
        file,
        source,
        Some(table),
        table.libraries.as_ref(),
        info,
        index,
        Some(&owners),
        Some(active),
        None,
        Some(&exact),
    )
}

fn publish_checked_local_signatures_selected(
    file: &File,
    source: crate::fir::SourceFileId,
    table: Option<&SymbolTable>,
    platform: &dyn crate::libraries::SemanticPlatform,
    info: &super::super::TypeInfo,
    index: &mut crate::fir::ResolvedModuleIndex,
    inline_owners: Option<&[crate::fir::DeclarationId]>,
    active_source: Option<&crate::fir::ActiveSourceDeclarations>,
    active_root: Option<(
        &crate::fir::ActiveSourceDeclarations,
        crate::fir::DeclarationId,
        &std::collections::HashSet<crate::fir::DeclarationId>,
    )>,
    exact_declarations: Option<&[crate::fir::DeclarationId]>,
) -> Result<(), Vec<crate::fir::DeclarationId>> {
    use crate::fir::{
        CallableId, DeclarationFlags, DeclarationId, DeclarationKind, PropertyId,
        ResolvedCallableShape, ResolvedTy, ResolvedTypeParameterFlags, ResolvedValueParameterFlags,
    };
    let active = active_source.or_else(|| active_root.map(|(active, _, _)| active));

    fn semantic_parameters(signature: &Signature) -> Vec<Ty> {
        signature
            .generic_sig
            .as_ref()
            .map(|generic| generic.params.clone())
            .unwrap_or_else(|| signature.params.clone())
    }

    fn semantic_result(signature: &Signature) -> Ty {
        signature
            .generic_sig
            .as_ref()
            .map_or(signature.ret, |generic| generic.ret)
    }

    fn enclosing_class<'a>(
        file: &'a File,
        index: &crate::fir::ResolvedModuleIndex,
        active: Option<&crate::fir::ActiveSourceDeclarations>,
        mut owner: Option<DeclarationId>,
    ) -> Option<(DeclarationId, DeclId, &'a ClassDecl)> {
        while let Some(declaration) = owner {
            let header = index.declaration_header(declaration)?;
            if header.kind == DeclarationKind::Classifier {
                if let Some(active) = active {
                    let (transient, class) = active.class(file, declaration)?;
                    return Some((declaration, transient, class));
                }
                let range = index.declaration_range(declaration)?;
                return file
                    .decl_arena
                    .iter()
                    .enumerate()
                    .find_map(|(raw, candidate)| match candidate {
                        Decl::Class(class) if class.span == range => {
                            Some((declaration, DeclId(raw as u32), class))
                        }
                        Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
                    });
            }
            owner = header.owner;
        }
        None
    }

    fn source_signature<'a>(
        class: &'a ClassSig,
        source: crate::libraries::SourceMember,
        method: &FunDecl,
        declaration: DeclarationId,
        active: bool,
    ) -> Option<(&'a Signature, Option<Ty>)> {
        if method.receiver.is_some() {
            class
                .member_ext_funs
                .get(&method.name)?
                .iter()
                .find(|candidate| {
                    if active {
                        candidate.signature.stable_declaration == Some(declaration)
                    } else {
                        candidate.signature.source_member == Some(source)
                    }
                })
                .map(|candidate| (&candidate.signature, Some(candidate.receiver_ty)))
        } else {
            class
                .methods
                .get(&method.name)?
                .iter()
                .find(|candidate| {
                    if active {
                        candidate.stable_declaration == Some(declaration)
                    } else {
                        candidate.source_member == Some(source)
                    }
                })
                .map(|candidate| (candidate, None))
        }
    }

    fn stable_parent(
        active: &crate::fir::ActiveSourceDeclarations,
        index: &crate::fir::ResolvedModuleIndex,
        candidate: DeclarationId,
    ) -> Option<DeclarationId> {
        let anchor = index.declaration_anchor(candidate)?;
        if anchor.kind == DeclarationKind::Classifier
            && index.declaration_header(candidate).is_none()
        {
            if let Some(canonical) = (0..index.declaration_count())
                .map(|raw| DeclarationId::from_raw(raw as u32))
                .find(|other| {
                    *other != candidate
                        && active.same_parser_declaration(*other, candidate)
                        && index
                            .declaration_header(*other)
                            .is_some_and(|header| header.flags.has(DeclarationFlags::LOCAL_CLASS))
                })
            {
                return Some(canonical);
            }
        }
        index
            .declaration_header(candidate)
            .and_then(|header| header.owner)
            .or(anchor.owner)
    }

    fn stable_ancestor_or_same(
        active: &crate::fir::ActiveSourceDeclarations,
        index: &crate::fir::ResolvedModuleIndex,
        ancestor: DeclarationId,
        descendant: DeclarationId,
    ) -> bool {
        let mut current = Some(descendant);
        while let Some(candidate) = current {
            if candidate == ancestor || active.same_parser_declaration(candidate, ancestor) {
                return true;
            }
            current = stable_parent(active, index, candidate);
        }
        false
    }

    fn local_interface_delegations(
        file: &File,
        info: &TypeInfo,
        index: &crate::fir::ResolvedModuleIndex,
        declaration: DeclarationId,
        transient: DeclId,
        class: &ClassDecl,
    ) -> Option<Vec<(Ty, crate::fir::ResolvedInterfaceDelegateSource)>> {
        let anonymous_construction =
            file.anonymous_object_classes
                .iter()
                .find_map(|(construction, candidate)| {
                    (*candidate == transient).then_some(*construction)
                });
        let anonymous_capture_count = anonymous_construction
            .and_then(|construction| {
                info.anonymous_object_captures_by_construction
                    .get(&construction)
            })
            .map_or(0, Vec::len);
        let checked_supertypes = info.checked_local_supertypes.get(&declaration);
        let mut synthetic_delegate_count = 0usize;
        class
            .interface_delegations
            .iter()
            .map(|delegation| {
                let supertype = usize::try_from(delegation.supertype?).ok()?;
                // A local classifier's persisted header owns captured type-parameter formals of
                // that classifier. The same written supertype checked at an anonymous-object
                // construction site is instantiated with enclosing lexical variables instead.
                // Prefer the stable declaration-form edge when Pass 1 published it; use the
                // Pass-2 checked edge only for a header that genuinely could not contain it.
                let interface = index
                    .classifier_header(declaration)
                    .and_then(|header| header.interfaces.get(supertype))
                    .map(|interface| interface.get())
                    .or_else(|| checked_supertypes?.get(supertype).copied())?;
                let parameter = delegation.bare_name.as_ref().and_then(|parameter_name| {
                    class
                        .props
                        .iter()
                        .position(|candidate| candidate.name == *parameter_name)
                });
                let source = match parameter {
                    Some(parameter) => {
                        crate::fir::ResolvedInterfaceDelegateSource::ConstructorParameter(
                            u32::try_from(parameter).ok()?,
                        )
                    }
                    None if anonymous_construction.is_some() => {
                        let parameter = anonymous_capture_count
                            .checked_add(synthetic_delegate_count)
                            .and_then(|parameter| u32::try_from(parameter).ok())?;
                        synthetic_delegate_count += 1;
                        crate::fir::ResolvedInterfaceDelegateSource::SyntheticConstructorParameter(
                            parameter,
                        )
                    }
                    None => crate::fir::ResolvedInterfaceDelegateSource::ConstructorBodyInitializer,
                };
                Some((interface, source))
            })
            .collect()
    }

    fn publish_checked_classifier_type_arguments(
        info: &TypeInfo,
        index: &mut crate::fir::ResolvedModuleIndex,
        platform: &dyn crate::libraries::SemanticPlatform,
        declaration: DeclarationId,
        class: &ClassDecl,
    ) -> bool {
        if index.classifier_type_arguments(declaration).is_some() {
            return true;
        }
        let Some(arguments) = info
            .checked_local_classifier_type_arguments
            .get(&declaration)
        else {
            return false;
        };
        let own_count = class.type_params.len();
        if arguments.len() < own_count {
            return false;
        }
        let is_interface = |index: &crate::fir::ResolvedModuleIndex, ty: Ty| {
            ty.non_null().obj_internal().is_some_and(|owner| {
                index
                    .classifier_declaration(owner)
                    .and_then(|declaration| index.declaration_header(declaration))
                    .is_some_and(|header| header.flags.has(DeclarationFlags::INTERFACE))
                    || platform
                        .classifier(owner)
                        .is_some_and(|classifier| classifier.is_interface())
            })
        };
        let semantic_own = info.resolved_declaration_type_parameters(class.span.lo);
        let mut parameters = Vec::with_capacity(arguments.len());
        for (ordinal, argument) in arguments.iter().copied().enumerate() {
            let Ty::TyParam(semantic_name, bound) = argument else {
                return false;
            };
            if ordinal < own_count {
                let parameter = if let Some(parameter) = index.type_parameter(
                    declaration,
                    u32::try_from(ordinal).expect("too many local classifier type parameters"),
                ) {
                    parameter
                } else {
                    let Some(expected_semantic_name) = semantic_own.get(ordinal) else {
                        return false;
                    };
                    if expected_semantic_name != semantic_name {
                        return false;
                    }
                    let source_name = &class.type_params[ordinal];
                    let mut bounds = class
                        .type_param_bounds
                        .iter()
                        .filter(|(formal, _)| formal == source_name)
                        .filter_map(|(_, reference)| info.resolved_type_bound(reference))
                        .collect::<Vec<_>>();
                    if bounds.is_empty() {
                        bounds.push((*bound, is_interface(index, *bound)));
                    }
                    let flags = ResolvedTypeParameterFlags::new(
                        class
                            .type_param_variances
                            .get(ordinal)
                            .copied()
                            .unwrap_or(crate::types::TypeVariance::Invariant),
                        false,
                        false,
                    );
                    let Ok(parameter) = index.publish_type_parameter(
                        declaration,
                        u32::try_from(ordinal).expect("too many local classifier type parameters"),
                        source_name,
                        semantic_name,
                        flags,
                        bounds,
                    ) else {
                        return false;
                    };
                    parameter
                };
                parameters.push(parameter);
                continue;
            }
            if let Some(parameter) = index.type_parameter_by_semantic_name(semantic_name) {
                parameters.push(parameter);
                continue;
            }
            let ordinal =
                u32::try_from(ordinal).expect("too many captured local classifier type parameters");
            let Ok(parameter) = index.publish_type_parameter(
                declaration,
                ordinal,
                crate::types::type_parameter_source_name(semantic_name),
                semantic_name,
                ResolvedTypeParameterFlags::new(
                    crate::types::TypeVariance::Invariant,
                    false,
                    false,
                ),
                [(*bound, is_interface(index, *bound))],
            ) else {
                return false;
            };
            parameters.push(parameter);
        }
        index.publish_classifier_type_arguments(
            declaration,
            u32::try_from(own_count).expect("too many own local classifier type parameters"),
            parameters,
        );
        true
    }

    fn publish_checked_classifier_hierarchy(
        index: &mut crate::fir::ResolvedModuleIndex,
        platform: &dyn crate::libraries::SemanticPlatform,
        source: crate::fir::SourceFileId,
        declaration: DeclarationId,
    ) -> bool {
        if index.classifier_hierarchy(declaration).is_some() {
            return true;
        }
        let Some(root) = index.classifier_self_type(declaration) else {
            return false;
        };
        let hierarchy = {
            let module = crate::fir::StreamedModuleSymbols::for_file(index, source.raw());
            let symbols = crate::symbol_source::CompositeSource::new(vec![
                &module as &dyn crate::symbol_source::SymbolSource,
                platform as &dyn crate::symbol_source::SymbolSource,
            ]);
            crate::symbol_resolver::applied_hierarchy(&symbols, root)
        };
        if hierarchy.is_empty() {
            return false;
        }
        index.publish_classifier_hierarchy(declaration, hierarchy);
        true
    }

    let local_declarations = index
        .local_class_declarations()
        .iter()
        .copied()
        .filter_map(|declaration| {
            let header = index.declaration_header(declaration)?;
            let selected_owner = match (inline_owners, exact_declarations) {
                (None, None) => true,
                (owners, exact) => {
                    let exact = exact.is_some_and(|exact| exact.contains(&declaration));
                    let subtree = owners.is_some_and(|owners| {
                        let mut current = Some(declaration);
                        while let Some(candidate) = current {
                            if owners.contains(&candidate) {
                                return true;
                            }
                            current = active
                                .and_then(|active| stable_parent(active, index, candidate))
                                .or_else(|| {
                                    index
                                        .declaration_anchor(candidate)
                                        .and_then(|anchor| anchor.owner)
                                });
                        }
                        false
                    });
                    exact || subtree
                }
            };
            let selected_active = active_root.is_none_or(|(active, _, bodies)| {
                bodies.iter().any(|body| {
                    stable_ancestor_or_same(active, index, declaration, *body)
                        || stable_ancestor_or_same(active, index, *body, declaration)
                        || header.owner.is_some_and(|owner| {
                            stable_ancestor_or_same(active, index, owner, *body)
                        })
                })
            });
            (selected_owner
                && selected_active
                && index
                    .declaration_anchor(declaration)
                    .is_some_and(|anchor| anchor.source == source))
            .then_some(declaration)
        })
        .collect::<Vec<_>>();
    crate::trace_compiler!(
        "fir",
        "publish checked local signatures source={source:?} inline_owners={inline_owners:?} active_root={:?} declarations={local_declarations:?} local_inventory={:?}",
        active_root.map(|(_, root, _)| root),
        (0..index.declaration_count())
            .map(|raw| DeclarationId::from_raw(raw as u32))
            .filter_map(|declaration| index
                .declaration_header(declaration)
                .filter(|header| header.flags.has(DeclarationFlags::LOCAL_CLASS))
                .map(|header| (declaration, index.declaration_anchor(declaration), header)))
            .collect::<Vec<_>>(),
    );
    let mut failed = Vec::new();
    let mut deferred_interface_delegations = Vec::new();

    let local_classifiers = local_declarations
        .iter()
        .copied()
        .filter(|declaration| {
            index
                .declaration_anchor(*declaration)
                .is_some_and(|anchor| anchor.kind == DeclarationKind::Classifier)
        })
        .collect::<Vec<_>>();
    for declaration in local_classifiers.iter().copied() {
        let Some(anchor) = index.declaration_anchor(declaration) else {
            failed.push(declaration);
            continue;
        };
        let class = table.and_then(|table| {
            table.classes.values().find(|candidate| {
                candidate.source_file == source.raw()
                    && match active {
                        Some(_) => candidate.stable_declaration == Some(declaration),
                        None => candidate.source_decl.is_some_and(|transient| {
                            file.decl_arena
                                .get(transient.0 as usize)
                                .is_some_and(|candidate| {
                                    matches!(candidate, Decl::Class(class) if Some(class.span) == index.declaration_range(declaration))
                                })
                        }),
                    }
            })
        });
        let parser_class = match active {
            Some(active) => active.class(file, declaration).or_else(|| {
                let transient = class?.source_decl?;
                match file.decl(transient) {
                    Decl::Class(class) => Some((transient, class)),
                    Decl::Fun(_) | Decl::Property(_) => None,
                }
            }),
            None => {
                let Some(range) = index.declaration_range(declaration) else {
                    failed.push(declaration);
                    continue;
                };
                file.decl_arena
                    .iter()
                    .enumerate()
                    .find_map(|(raw, candidate)| match candidate {
                        Decl::Class(class) if class.span == range => {
                            Some((DeclId(raw as u32), class))
                        }
                        Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
                    })
            }
        };
        let Some((transient, class_decl)) = parser_class else {
            crate::trace_compiler!(
                "signature",
                "local classifier has no active parser binding declaration={declaration:?} anchor={anchor:?}",
            );
            failed.push(declaration);
            continue;
        };
        let class = class.or_else(|| {
            table.and_then(|table| {
                table.classes.values().find(|candidate| {
                    candidate.source_file == source.raw()
                        && candidate.source_decl == Some(transient)
                })
            })
        });
        let Some(class) = class else {
            // Production Pass 2 has destroyed the parser-backed signature graph. A body-local
            // classifier's already-published primary-constructor result carries its stable semantic
            // identity, while the authoritative body check carries the exact lexical supertypes.
            // Publish that checked header directly; never require a retained `ClassSig` merely to
            // join the active parser declaration back to its classifier.
            let classifier = info
                .checked_local_classifier_identities
                .get(&declaration)
                .copied()
                .or_else(|| {
                    (0..index.declaration_count()).find_map(|raw| {
                        let child = DeclarationId::from_raw(raw as u32);
                        let child_anchor = index.declaration_anchor(child)?;
                        (child_anchor.owner == Some(declaration)
                            && child_anchor.kind == DeclarationKind::Constructor)
                            .then(|| index.signature(child)?.result.get().kotlin_class_internal())
                            .flatten()
                    })
                });
            let Some(classifier) = classifier else {
                crate::trace_compiler!(
                    "signature",
                    "local classifier has no checked identity declaration={declaration:?} transient={transient:?}",
                );
                failed.push(declaration);
                continue;
            };
            let checked_supertypes = info
                .checked_local_supertypes
                .get(&declaration)
                .map(|supertypes| supertypes.to_vec())
                .unwrap_or_else(|| {
                    class_decl
                        .supertypes
                        .iter()
                        .filter_map(|supertype| info.resolved_type(supertype))
                        .collect()
                });
            let (superclass, interfaces) = {
                let is_interface = |ty: Ty| {
                    let Some(owner) = ty.non_null().kotlin_class_internal() else {
                        return false;
                    };
                    index
                        .classifier_declaration(owner)
                        .and_then(|declaration| index.declaration_header(declaration))
                        .is_some_and(|header| header.flags.has(DeclarationFlags::INTERFACE))
                        || platform
                            .classifier(owner)
                            .is_some_and(|classifier| classifier.is_interface())
                };
                let superclass = (!class_decl.is_interface())
                    .then(|| {
                        checked_supertypes
                            .iter()
                            .copied()
                            .find(|supertype| !is_interface(*supertype))
                    })
                    .flatten();
                let interfaces = checked_supertypes
                    .into_iter()
                    .filter(|supertype| is_interface(*supertype))
                    .collect::<Vec<_>>();
                (superclass, interfaces)
            };
            if index.classifier_header(declaration).is_none()
                && index
                    .publish_classifier_header(
                        declaration,
                        classifier,
                        superclass,
                        interfaces,
                        std::iter::empty(),
                        std::iter::empty(),
                        std::iter::empty(),
                    )
                    .is_err()
            {
                failed.push(declaration);
                continue;
            }
            let Some(interface_delegations) =
                local_interface_delegations(file, info, index, declaration, transient, class_decl)
            else {
                failed.push(declaration);
                continue;
            };
            if !interface_delegations.is_empty() {
                deferred_interface_delegations.push((
                    declaration,
                    classifier,
                    interface_delegations,
                ));
            }
            if !publish_checked_classifier_type_arguments(
                info,
                index,
                platform,
                declaration,
                class_decl,
            ) {
                failed.push(declaration);
                continue;
            }
            if !publish_checked_classifier_hierarchy(index, platform, source, declaration) {
                failed.push(declaration);
            }
            continue;
        };
        let superclass = class
            .super_internal
            .map(|owner| Ty::obj_args_name(owner, &class.super_type_args));
        let interfaces = class
            .interfaces
            .iter()
            .enumerate()
            .map(|(ordinal, owner)| {
                Ty::obj_args_name(
                    owner,
                    class
                        .interface_type_args
                        .get(ordinal)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        if index.classifier_header(declaration).is_none() {
            let sealed_subclasses = class
                .is_sealed()
                .then(|| {
                    table
                        .expect("legacy local classifier publication requires Pass-1 symbols")
                        .subclass_names_of(class.internal)
                })
                .unwrap_or_default();
            if index
                .publish_classifier_header(
                    declaration,
                    class.internal,
                    superclass,
                    interfaces,
                    std::iter::empty(),
                    std::iter::empty(),
                    sealed_subclasses,
                )
                .is_err()
            {
                failed.push(declaration);
                continue;
            }
        }
        let Some(interface_delegations) =
            local_interface_delegations(file, info, index, declaration, transient, class_decl)
        else {
            failed.push(declaration);
            continue;
        };
        if !interface_delegations.is_empty() {
            deferred_interface_delegations.push((
                declaration,
                class.internal,
                interface_delegations,
            ));
        }
        let own = class_decl
            .type_params
            .iter()
            .zip(class.type_parameters.type_params.iter())
            .zip(class.type_parameters.type_param_bounds.iter())
            .zip(class.type_parameters.type_param_variances.iter())
            .map(|(((source_name, semantic_name), bound), variance)| {
                (
                    source_name.as_str(),
                    semantic_name.as_str(),
                    *bound,
                    *variance,
                )
            });
        let captured = class
            .captured_type_parameters
            .type_params
            .iter()
            .zip(class.captured_type_parameters.type_param_bounds.iter())
            .map(|(semantic_name, bound)| {
                (
                    crate::types::type_parameter_source_name(semantic_name),
                    semantic_name.as_str(),
                    *bound,
                    crate::types::TypeVariance::Invariant,
                )
            });
        let type_argument_count = class.type_parameters.type_params.len()
            + class.captured_type_parameters.type_params.len();
        for (ordinal, (source_name, semantic_name, bound, variance)) in
            own.chain(captured).enumerate()
        {
            if index.type_parameter(declaration, ordinal as u32).is_some() {
                continue;
            }
            let is_interface = bound.non_null().obj_internal().is_some_and(|owner| {
                table
                    .and_then(|table| table.classes.get(&owner))
                    .is_some_and(ClassSig::is_interface)
                    || platform
                        .classifier(owner)
                        .is_some_and(|classifier| classifier.is_interface())
            });
            if index
                .publish_type_parameter(
                    declaration,
                    ordinal as u32,
                    source_name,
                    semantic_name,
                    ResolvedTypeParameterFlags::new(variance, false, false),
                    [(bound, is_interface)],
                )
                .is_err()
            {
                failed.push(declaration);
                break;
            }
        }
        let type_arguments = (0..type_argument_count)
            .map(|ordinal| index.type_parameter(declaration, ordinal as u32))
            .collect::<Option<Vec<_>>>();
        if index.classifier_type_arguments(declaration).is_some() {
            continue;
        } else if let Some(type_arguments) = type_arguments {
            index.publish_classifier_type_arguments(
                declaration,
                u32::try_from(class.type_parameters.type_params.len())
                    .expect("too many own local classifier type parameters"),
                type_arguments,
            );
        } else if type_argument_count != 0 {
            failed.push(declaration);
        }
    }

    for declaration in local_declarations {
        let signature_published = index.signature(declaration).is_some();
        let Some(anchor) = index.declaration_anchor(declaration) else {
            failed.push(declaration);
            continue;
        };
        if anchor.kind == DeclarationKind::Classifier {
            continue;
        }
        if anchor.kind == DeclarationKind::Property
            && index
                .declaration_header(declaration)
                .is_some_and(|header| header.flags.has(DeclarationFlags::COMPILER_GENERATED))
        {
            // Anonymous/local capture storage is already carried by checked FIR capture
            // coordinates. It is not a Kotlin property signature and has no source declaration
            // from which this active lexical publication could manufacture one.
            continue;
        }
        if matches!(
            anchor.kind,
            DeclarationKind::EnumEntry
                | DeclarationKind::TypeAlias
                | DeclarationKind::Initializer
                | DeclarationKind::Script
        ) {
            continue;
        }
        let already_complete = match anchor.kind {
            DeclarationKind::Function
            | DeclarationKind::Constructor
            | DeclarationKind::Accessor => {
                signature_published && index.callable_for_declaration(declaration).is_some()
            }
            DeclarationKind::Property => {
                signature_published && index.property_for_declaration(declaration).is_some()
            }
            DeclarationKind::Classifier
            | DeclarationKind::EnumEntry
            | DeclarationKind::TypeAlias
            | DeclarationKind::Initializer
            | DeclarationKind::Script => false,
        };
        if already_complete {
            continue;
        }
        let Some((owner_stable, owner_decl, owner)) =
            enclosing_class(file, index, active, anchor.owner)
        else {
            crate::trace_compiler!(
                "signature",
                "local declaration has no active classifier declaration={declaration:?} anchor={anchor:?} owner_binding={:?}",
                anchor.owner.and_then(|owner| active.and_then(|active| active.class(file, owner)))
            );
            continue;
        };
        let class = table.and_then(|table| {
            table.classes.values().find(|candidate| {
                candidate.source_file == source.raw()
                    && match active {
                        Some(_) => candidate.stable_declaration == Some(owner_stable),
                        None => candidate.source_decl == Some(owner_decl),
                    }
            })
        });

        if class.is_none() && active.is_some() {
            // The production streaming path deliberately owns no Pass-1 `ClassSig`. Publish an
            // active local member from the declaration syntax plus facts produced by its checked
            // lexical body. This is declaration publication, not lookup: every target identity is
            // the stable declaration currently being consumed.
            match anchor.kind {
                DeclarationKind::Function => {
                    let Some(method) = active.and_then(|active| active.function(file, declaration))
                    else {
                        failed.push(declaration);
                        continue;
                    };
                    let parameters = method
                        .params
                        .iter()
                        .map(|parameter| {
                            info.resolved_declaration_type(&parameter.ty)
                                .or_else(|| info.resolved_type(&parameter.ty))
                                .map(|ty| {
                                    crate::types::semantic_value_parameter_ty(
                                        ty,
                                        parameter.is_vararg,
                                    )
                                })
                        })
                        .collect::<Option<Vec<_>>>();
                    let result = method
                        .ret
                        .as_ref()
                        .and_then(|result| {
                            info.resolved_declaration_type(result)
                                .or_else(|| info.resolved_type(result))
                        })
                        .or_else(|| info.checked_declaration_results.get(&declaration).copied())
                        .or_else(|| {
                            super::super::fun_body_expr(&method.body)
                                .and_then(|body| info.expr_types.get(body.0 as usize).copied())
                        });
                    let (Some(parameters), Some(result)) = (parameters, result) else {
                        failed.push(declaration);
                        continue;
                    };
                    if parameters
                        .iter()
                        .chain(std::iter::once(&result))
                        .any(|ty| ty.mentions_error() || ty.mentions_pending())
                    {
                        failed.push(declaration);
                        continue;
                    }
                    let semantic_names =
                        info.resolved_declaration_type_parameters(method.signature_span.lo);
                    let mut type_parameters_failed =
                        semantic_names.len() != method.type_params.len();
                    for (ordinal, source_name) in method.type_params.iter().enumerate() {
                        if index.type_parameter(declaration, ordinal as u32).is_some() {
                            continue;
                        }
                        let Some(semantic_name) = semantic_names.get(ordinal) else {
                            type_parameters_failed = true;
                            break;
                        };
                        let bounds = method
                            .type_param_bounds
                            .iter()
                            .filter(|(formal, _)| formal == source_name)
                            .filter_map(|(_, bound)| info.resolved_type_bound(bound));
                        if index
                            .publish_type_parameter(
                                declaration,
                                ordinal as u32,
                                source_name,
                                semantic_name,
                                ResolvedTypeParameterFlags::new(
                                    crate::types::TypeVariance::Invariant,
                                    false,
                                    method.reified_type_params.contains(source_name),
                                ),
                                bounds,
                            )
                            .is_err()
                        {
                            type_parameters_failed = true;
                            break;
                        }
                    }
                    if type_parameters_failed
                        || (!signature_published
                            && index
                                .publish_signature(declaration, parameters, result)
                                .is_err())
                    {
                        failed.push(declaration);
                        continue;
                    }
                    let receiver = method
                        .receiver
                        .as_ref()
                        .and_then(|receiver| {
                            info.resolved_declaration_type(receiver)
                                .or_else(|| info.resolved_type(receiver))
                        })
                        .map(ResolvedTy::new)
                        .transpose();
                    let Ok(receiver) = receiver else {
                        failed.push(declaration);
                        continue;
                    };
                    let callable = CallableId::from_raw(declaration.raw());
                    index.publish_function_shape(
                        callable,
                        declaration,
                        &method.name,
                        ResolvedCallableShape {
                            context_parameter_count: method.context_count as u32,
                            context_value_count: method.context_value_count() as u32,
                            extension_receiver: receiver,
                        },
                        method.is_inline(),
                    );
                    index.publish_callable_parameters(
                        callable,
                        method.params.iter().map(|parameter| {
                            (
                                parameter.name.as_str(),
                                ResolvedValueParameterFlags::new(
                                    parameter.is_vararg,
                                    parameter.default.is_some(),
                                    false,
                                    false,
                                ),
                            )
                        }),
                    );
                }
                DeclarationKind::Property => {
                    if let Some(parameter) =
                        active.and_then(|active| active.constructor_parameter(file, declaration))
                    {
                        let ty = info
                            .resolved_declaration_type(&parameter.ty)
                            .or_else(|| info.resolved_type(&parameter.ty))
                            .map(|ty| {
                                crate::types::semantic_value_parameter_ty(ty, parameter.is_vararg)
                            });
                        let Some(ty) =
                            ty.filter(|ty| !ty.mentions_error() && !ty.mentions_pending())
                        else {
                            failed.push(declaration);
                            continue;
                        };
                        if !signature_published
                            && index.publish_signature(declaration, [], ty).is_err()
                        {
                            failed.push(declaration);
                            continue;
                        }
                        index.publish_property_shape(
                            PropertyId::from_raw(declaration.raw()),
                            declaration,
                            0,
                            0,
                            None,
                            parameter.is_var,
                        );
                        continue;
                    }
                    let property = active.and_then(|active| active.property(file, declaration));
                    let Some(property) = property else {
                        failed.push(declaration);
                        continue;
                    };
                    let ty = property
                        .declared_ty()
                        .and_then(|declared| {
                            info.resolved_declaration_type(declared)
                                .or_else(|| info.resolved_type(declared))
                        })
                        .or_else(|| {
                            info.property_decl_types
                                .get(&(property.span.lo, property.span.hi))
                                .copied()
                        })
                        .or_else(|| {
                            property
                                .init
                                .and_then(|init| info.expr_types.get(init.0 as usize).copied())
                        })
                        .or_else(|| {
                            property
                                .getter
                                .as_ref()
                                .and_then(super::super::fun_body_expr)
                                .and_then(|body| info.expr_types.get(body.0 as usize).copied())
                        });
                    let Some(ty) = ty.filter(|ty| !ty.mentions_error() && !ty.mentions_pending())
                    else {
                        failed.push(declaration);
                        continue;
                    };
                    if !signature_published && index.publish_signature(declaration, [], ty).is_err()
                    {
                        failed.push(declaration);
                        continue;
                    }
                    let receiver = property
                        .receiver
                        .as_ref()
                        .and_then(|receiver| {
                            info.resolved_declaration_type(receiver)
                                .or_else(|| info.resolved_type(receiver))
                        })
                        .map(ResolvedTy::new)
                        .transpose();
                    let Ok(receiver) = receiver else {
                        failed.push(declaration);
                        continue;
                    };
                    index.publish_property_shape(
                        PropertyId::from_raw(declaration.raw()),
                        declaration,
                        property.context_params.len() as u32,
                        property.context_value_count() as u32,
                        receiver,
                        property.is_var,
                    );
                }
                DeclarationKind::Accessor => {
                    let Some(property_declaration) = anchor.owner else {
                        failed.push(declaration);
                        continue;
                    };
                    let Some(property_id) = index.property_for_declaration(property_declaration)
                    else {
                        failed.push(declaration);
                        continue;
                    };
                    let Some(property_header) = index.property(property_id) else {
                        failed.push(declaration);
                        continue;
                    };
                    let Some(property_signature) = index.signature(property_declaration).cloned()
                    else {
                        failed.push(declaration);
                        continue;
                    };
                    let is_setter = anchor.sibling == 1;
                    let mut parameters = property_signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>();
                    if is_setter {
                        parameters.push(property_signature.result.get());
                    }
                    if !signature_published
                        && index
                            .publish_signature(
                                declaration,
                                parameters,
                                if is_setter {
                                    Ty::Unit
                                } else {
                                    property_signature.result.get()
                                },
                            )
                            .is_err()
                    {
                        failed.push(declaration);
                        continue;
                    }
                    let Some(property_name) = index
                        .declaration_name(property_declaration)
                        .map(str::to_owned)
                    else {
                        failed.push(declaration);
                        continue;
                    };
                    let callable_name = if is_setter {
                        crate::names::property_setter_name(&property_name)
                    } else {
                        crate::names::property_getter_name(&property_name)
                    };
                    index.publish_function_shape(
                        CallableId::from_raw(declaration.raw()),
                        declaration,
                        &callable_name,
                        ResolvedCallableShape {
                            context_parameter_count: property_header.context_parameter_count,
                            context_value_count: property_header.context_value_count,
                            extension_receiver: property_header.extension_receiver,
                        },
                        index
                            .declaration_header(declaration)
                            .is_some_and(|header| header.flags.has(DeclarationFlags::INLINE)),
                    );
                }
                DeclarationKind::Constructor => {
                    let mut parameters = Vec::new();
                    let mut source_parameters = Vec::new();
                    let mut parameter_failed = false;
                    for parameter in &owner.context_params {
                        let Some(ty) = info
                            .resolved_declaration_type(&parameter.ty)
                            .or_else(|| info.resolved_type(&parameter.ty))
                        else {
                            parameter_failed = true;
                            break;
                        };
                        parameters.push(crate::types::semantic_value_parameter_ty(
                            ty,
                            parameter.is_vararg,
                        ));
                        source_parameters.push((
                            parameter.name.as_str(),
                            parameter.is_vararg,
                            parameter.default.is_some(),
                            false,
                            false,
                        ));
                    }
                    if parameter_failed {
                        failed.push(declaration);
                        continue;
                    }
                    if anchor.sibling == 0 {
                        for parameter in &owner.props {
                            let Some(ty) = info
                                .resolved_declaration_type(&parameter.ty)
                                .or_else(|| info.resolved_type(&parameter.ty))
                            else {
                                parameter_failed = true;
                                break;
                            };
                            parameters.push(crate::types::semantic_value_parameter_ty(
                                ty,
                                parameter.is_vararg,
                            ));
                            source_parameters.push((
                                parameter.name.as_str(),
                                parameter.is_vararg,
                                parameter.default.is_some(),
                                parameter.is_property,
                                parameter.is_var,
                            ));
                        }
                        if parameter_failed {
                            failed.push(declaration);
                            continue;
                        }
                        let captures = info
                            .anonymous_object_captures_by_class
                            .get(&owner_decl)
                            .or_else(|| info.local_class_captures_by_class.get(&owner_decl));
                        for capture in captures.into_iter().flatten() {
                            parameters.push(capture.stored_ty());
                            source_parameters.push((
                                capture.name.as_str(),
                                false,
                                false,
                                true,
                                false,
                            ));
                        }
                    } else {
                        let Some(constructor) = owner
                            .secondary_ctors
                            .get(anchor.sibling.saturating_sub(1) as usize)
                        else {
                            failed.push(declaration);
                            continue;
                        };
                        for parameter in &constructor.params {
                            let Some(ty) = info
                                .resolved_declaration_type(&parameter.ty)
                                .or_else(|| info.resolved_type(&parameter.ty))
                            else {
                                parameter_failed = true;
                                break;
                            };
                            parameters.push(crate::types::semantic_value_parameter_ty(
                                ty,
                                parameter.is_vararg,
                            ));
                            source_parameters.push((
                                parameter.name.as_str(),
                                parameter.is_vararg,
                                parameter.default.is_some(),
                                false,
                                false,
                            ));
                        }
                        if parameter_failed {
                            failed.push(declaration);
                            continue;
                        }
                    }
                    if parameters
                        .iter()
                        .any(|parameter| parameter.mentions_error() || parameter.mentions_pending())
                    {
                        failed.push(declaration);
                        continue;
                    }
                    let Some(result) = index.classifier_self_type(owner_stable) else {
                        failed.push(declaration);
                        continue;
                    };
                    if !signature_published
                        && index
                            .publish_signature(declaration, parameters, result)
                            .is_err()
                    {
                        failed.push(declaration);
                        continue;
                    }
                    let callable = CallableId::from_raw(declaration.raw());
                    index.publish_constructor_shape(
                        callable,
                        declaration,
                        ResolvedCallableShape {
                            context_parameter_count: owner.context_params.len() as u32,
                            context_value_count: owner
                                .context_params
                                .iter()
                                .filter(|parameter| parameter.name != "_")
                                .count() as u32,
                            extension_receiver: None,
                        },
                    );
                    index.publish_callable_parameters(
                        callable,
                        source_parameters.into_iter().map(|parameter| {
                            (
                                parameter.0,
                                ResolvedValueParameterFlags::new(
                                    parameter.1,
                                    parameter.2,
                                    parameter.3,
                                    parameter.4,
                                ),
                            )
                        }),
                    );
                }
                DeclarationKind::Classifier
                | DeclarationKind::EnumEntry
                | DeclarationKind::TypeAlias
                | DeclarationKind::Initializer
                | DeclarationKind::Script => {}
            }
            continue;
        }
        let Some(class) = class else {
            failed.push(declaration);
            continue;
        };

        let generated = index
            .declaration_header(declaration)
            .is_some_and(|header| header.flags.has(DeclarationFlags::COMPILER_GENERATED));
        if generated && anchor.kind == DeclarationKind::Function {
            let Some(signature) = class
                .methods
                .values()
                .flatten()
                .find(|signature| signature.stable_declaration == Some(declaration))
            else {
                failed.push(declaration);
                continue;
            };
            if !signature_published
                && index
                    .publish_signature(
                        declaration,
                        semantic_parameters(signature),
                        semantic_result(signature),
                    )
                    .is_err()
            {
                failed.push(declaration);
                continue;
            }
            if index.callable_for_declaration(declaration).is_some() {
                continue;
            }
            let Some(name) = index.declaration_name(declaration).map(str::to_owned) else {
                failed.push(declaration);
                continue;
            };
            let parameter_names = signature.param_names.clone();
            let parameter_defaults = signature.param_defaults.clone();
            let vararg_index = signature.vararg_index;
            let callable = CallableId::from_raw(declaration.raw());
            index.publish_function(callable, declaration, &name, false);
            index.publish_callable_parameters(
                callable,
                parameter_names.iter().enumerate().map(|(ordinal, name)| {
                    (
                        name.as_str(),
                        ResolvedValueParameterFlags::new(
                            vararg_index == Some(ordinal),
                            parameter_defaults.get(ordinal).copied().unwrap_or(false),
                            false,
                            false,
                        ),
                    )
                }),
            );
            continue;
        }

        match anchor.kind {
            DeclarationKind::Function => {
                let method = match active {
                    Some(active) => active.function(file, declaration),
                    None => owner.methods.get(anchor.sibling as usize),
                };
                let Some(method) = method else {
                    failed.push(declaration);
                    continue;
                };
                let source_member = crate::libraries::SourceMember::Class {
                    file: source.raw(),
                    owner: owner_decl.0,
                    method: anchor.sibling,
                };
                let Some((signature, receiver)) =
                    source_signature(class, source_member, method, declaration, active.is_some())
                else {
                    crate::trace_compiler!(
                        "signature",
                        "missing local source signature declaration={declaration:?} expected={source_member:?} owner={:?} method={} candidates={:?}",
                        class.internal,
                        method.name,
                        class
                            .methods
                            .get(&method.name)
                            .into_iter()
                            .flatten()
                            .map(|candidate| candidate.source_member)
                            .collect::<Vec<_>>(),
                    );
                    failed.push(declaration);
                    continue;
                };
                let semantic_type_parameters = info
                    .resolved_declaration_type_parameters
                    .get(&method.signature_span.lo);
                let generic = signature.generic_sig.as_ref();
                let mut type_parameters_failed = false;
                for (ordinal, source_name) in method.type_params.iter().enumerate() {
                    if index.type_parameter(declaration, ordinal as u32).is_some() {
                        continue;
                    }
                    let Some(semantic_name) = semantic_type_parameters
                        .and_then(|parameters| parameters.get(ordinal))
                        .or_else(|| generic.and_then(|generic| generic.formals.get(ordinal)))
                    else {
                        type_parameters_failed = true;
                        break;
                    };
                    let bounds = generic
                        .and_then(|generic| generic.formal_bounds.get(ordinal))
                        .cloned()
                        .unwrap_or_default();
                    let resolved_bounds = bounds.into_iter().map(|bound| {
                        let is_interface = bound.non_null().obj_internal().is_some_and(|owner| {
                            table
                                .and_then(|table| table.classes.get(&owner))
                                .is_some_and(ClassSig::is_interface)
                                || platform
                                    .classifier(owner)
                                    .is_some_and(|classifier| classifier.is_interface())
                        });
                        (bound, is_interface)
                    });
                    if index
                        .publish_type_parameter(
                            declaration,
                            ordinal as u32,
                            source_name,
                            semantic_name,
                            ResolvedTypeParameterFlags::new(
                                crate::types::TypeVariance::Invariant,
                                false,
                                method.reified_type_params.contains(source_name),
                            ),
                            resolved_bounds,
                        )
                        .is_err()
                    {
                        type_parameters_failed = true;
                        break;
                    }
                }
                if type_parameters_failed {
                    failed.push(declaration);
                    continue;
                }
                // Pass 1 deliberately leaves local-class member slots unresolved when they depend
                // on enclosing body-local type parameters. Pass 2 has checked those declaration
                // types in the exact lexical scope; overlay those facts on the compact signature
                // instead of publishing its temporary Error placeholders.
                let mut parameters = semantic_parameters(signature);
                let mut parameters_failed = false;
                for (ordinal, parameter) in method.params.iter().enumerate() {
                    let Some(checked) = info.resolved_declaration_type(&parameter.ty) else {
                        continue;
                    };
                    // `FunDecl::params` already stores context parameters as its leading entries,
                    // matching `Signature::params`. Adding `context_count` here a second time
                    // shifted every ordinary parameter and made the final one unpublishable.
                    let Some(slot) = parameters.get_mut(ordinal) else {
                        failed.push(declaration);
                        parameters_failed = true;
                        break;
                    };
                    *slot = crate::types::semantic_value_parameter_ty(checked, parameter.is_vararg);
                }
                if parameters_failed {
                    continue;
                }
                let result = method
                    .ret
                    .as_ref()
                    .and_then(|result| info.resolved_declaration_type(result))
                    .or_else(|| info.checked_declaration_results.get(&declaration).copied())
                    .or_else(|| {
                        info.checked_source_member_results
                            .get(&source_member)
                            .copied()
                    })
                    .unwrap_or_else(|| semantic_result(signature));
                crate::trace_compiler!(
                    "signature",
                    "publish local function declaration={declaration:?} source={source_member:?} parameters={:?} result={result:?}",
                    parameters,
                );
                if !signature_published
                    && index
                        .publish_signature(declaration, parameters, result)
                        .is_err()
                {
                    failed.push(declaration);
                    continue;
                }
                let receiver = receiver.map(ResolvedTy::new).transpose();
                let Ok(receiver) = receiver else {
                    failed.push(declaration);
                    continue;
                };
                if index.callable_for_declaration(declaration).is_some() {
                    continue;
                }
                let callable = CallableId::from_raw(declaration.raw());
                index.publish_function_shape(
                    callable,
                    declaration,
                    &method.name,
                    ResolvedCallableShape {
                        context_parameter_count: signature.context_count as u32,
                        context_value_count: method.context_value_count() as u32,
                        extension_receiver: receiver,
                    },
                    method.is_inline(),
                );
                index.publish_callable_parameters(
                    callable,
                    method.params.iter().map(|parameter| {
                        (
                            parameter.name.as_str(),
                            ResolvedValueParameterFlags::new(
                                parameter.is_vararg,
                                parameter.default.is_some(),
                                false,
                                false,
                            ),
                        )
                    }),
                );
            }
            DeclarationKind::Constructor => {
                let (parameters, mut source_parameters) = if anchor.sibling == 0 {
                    (
                        class
                            .ctor_param_shapes
                            .iter()
                            .map(|(parameter, _)| *parameter)
                            .collect::<Vec<_>>(),
                        owner
                            .props
                            .iter()
                            .map(|parameter| {
                                (
                                    parameter.name.as_str(),
                                    parameter.is_vararg,
                                    parameter.default.is_some(),
                                    parameter.is_property,
                                    parameter.is_var,
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                } else {
                    let index = anchor.sibling.saturating_sub(1) as usize;
                    let Some(parameters) = class.secondary_ctors.get(index) else {
                        failed.push(declaration);
                        continue;
                    };
                    let Some(constructor) = owner.secondary_ctors.get(index) else {
                        failed.push(declaration);
                        continue;
                    };
                    (
                        class
                            .secondary_ctor_shapes
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| parameters.clone()),
                        constructor
                            .params
                            .iter()
                            .map(|parameter| {
                                (
                                    parameter.name.as_str(),
                                    parameter.is_vararg,
                                    parameter.default.is_some(),
                                    false,
                                    false,
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                };
                if anchor.sibling == 0 && source_parameters.len() < parameters.len() {
                    let capture_count = parameters.len() - source_parameters.len();
                    source_parameters.extend(
                        info.anonymous_object_captures_by_class
                            .get(&owner_decl)
                            .into_iter()
                            .flatten()
                            .take(capture_count)
                            .map(|capture| (capture.name.as_str(), false, false, true, false)),
                    );
                }
                if !signature_published
                    && index
                        .publish_signature(declaration, parameters, semantic_classifier_self(class))
                        .is_err()
                {
                    failed.push(declaration);
                    continue;
                }
                if index.callable_for_declaration(declaration).is_some() {
                    continue;
                }
                let callable = CallableId::from_raw(declaration.raw());
                index.publish_constructor(callable, declaration);
                index.publish_callable_parameters(
                    callable,
                    source_parameters.iter().map(|parameter| {
                        (
                            parameter.0,
                            ResolvedValueParameterFlags::new(
                                parameter.1,
                                parameter.2,
                                parameter.3,
                                parameter.4,
                            ),
                        )
                    }),
                );
            }
            DeclarationKind::Property => {
                let generated_property = generated.then(|| {
                    class
                        .declared_props
                        .values()
                        .find(|property| property.stable_declaration == Some(declaration))
                });
                let property = match active {
                    Some(active) => active.property(file, declaration),
                    None => owner.body_props.iter().find(|property| {
                        Some(property.span) == index.declaration_range(declaration)
                    }),
                };
                let parameter = match active {
                    Some(active) => active.constructor_parameter(file, declaration),
                    None => owner.props.iter().find(|parameter| {
                        Some(parameter.span) == index.declaration_range(declaration)
                            && parameter.is_property
                    }),
                };
                crate::trace_compiler!(
                    "signature",
                    "publish local property declaration={declaration:?} generated={generated} body_property={:?} parameter={:?} checked_type={:?} declared_properties={:?}",
                    property.map(|property| (&property.name, property.span)),
                    parameter.map(|parameter| (&parameter.name, parameter.span)),
                    property.and_then(|property| info
                        .property_decl_types
                        .get(&(property.span.lo, property.span.hi))),
                    class.declared_props.keys().collect::<Vec<_>>(),
                );
                let (ty, context_count, context_value_count, receiver, mutable) =
                    if let Some(Some(candidate)) = generated_property {
                        (
                            candidate.ty,
                            candidate.context_params.len(),
                            candidate.context_params.len(),
                            None,
                            false,
                        )
                    } else if let Some(property) = property {
                        if property.receiver.is_some() {
                            let Some(candidate) = class
                                .member_ext_props
                                .get(&property.name)
                                .and_then(|candidates| {
                                    candidates.iter().find(|candidate| {
                                        candidate.stable_declaration() == Some(declaration)
                                    })
                                })
                            else {
                                failed.push(declaration);
                                continue;
                            };
                            (
                                candidate.ret(),
                                candidate.context_params().len(),
                                property.context_value_count(),
                                Some(candidate.receiver_ty()),
                                property.is_var,
                            )
                        } else {
                            let Some(candidate) = class.declared_props.get(&property.name) else {
                                failed.push(declaration);
                                continue;
                            };
                            (
                                info.property_decl_types
                                    .get(&(property.span.lo, property.span.hi))
                                    .copied()
                                    .unwrap_or(candidate.ty),
                                candidate.context_params.len(),
                                property.context_value_count(),
                                None,
                                property.is_var,
                            )
                        }
                    } else if let Some(parameter) = parameter {
                        let Some(candidate) = class.declared_props.get(&parameter.name) else {
                            failed.push(declaration);
                            continue;
                        };
                        let parameter_index = owner
                            .props
                            .iter()
                            .position(|candidate| std::ptr::eq(candidate, parameter));
                        let ty = parameter_index
                            .and_then(|index| class.ctor_param_shapes.get(index))
                            .map_or(candidate.ty, |(shape, _)| *shape);
                        (
                            ty,
                            candidate.context_params.len(),
                            0,
                            None,
                            parameter.is_var,
                        )
                    } else {
                        failed.push(declaration);
                        continue;
                    };
                crate::trace_compiler!(
                    "signature",
                    "publish local property signature declaration={declaration:?} type={ty:?} receiver={receiver:?}",
                );
                if !signature_published && index.publish_signature(declaration, [], ty).is_err() {
                    failed.push(declaration);
                    continue;
                }
                let receiver = receiver.map(ResolvedTy::new).transpose();
                let Ok(receiver) = receiver else {
                    failed.push(declaration);
                    continue;
                };
                if index.property_for_declaration(declaration).is_none() {
                    index.publish_property_shape(
                        PropertyId::from_raw(declaration.raw()),
                        declaration,
                        context_count as u32,
                        context_value_count as u32,
                        receiver,
                        mutable,
                    );
                }
            }
            DeclarationKind::Accessor => {
                let Some(property_declaration) = anchor.owner else {
                    failed.push(declaration);
                    continue;
                };
                let Some(property_id) = index.property_for_declaration(property_declaration) else {
                    failed.push(declaration);
                    continue;
                };
                let Some(property_header) = index.property(property_id) else {
                    failed.push(declaration);
                    continue;
                };
                let Some(property_signature) = index.signature(property_declaration).cloned()
                else {
                    failed.push(declaration);
                    continue;
                };
                let is_setter = anchor.sibling == 1;
                if !signature_published {
                    let mut parameters = property_signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>();
                    if is_setter {
                        parameters.push(property_signature.result.get());
                    }
                    if index
                        .publish_signature(
                            declaration,
                            parameters,
                            if is_setter {
                                Ty::Unit
                            } else {
                                property_signature.result.get()
                            },
                        )
                        .is_err()
                    {
                        failed.push(declaration);
                        continue;
                    }
                }
                if index.callable_for_declaration(declaration).is_some() {
                    continue;
                }
                let Some(property_name) = index
                    .declaration_name(property_declaration)
                    .map(str::to_owned)
                else {
                    failed.push(declaration);
                    continue;
                };
                let callable_name = if is_setter {
                    crate::names::property_setter_name(&property_name)
                } else {
                    crate::names::property_getter_name(&property_name)
                };
                let callable = CallableId::from_raw(declaration.raw());
                index.publish_function_shape(
                    callable,
                    declaration,
                    &callable_name,
                    ResolvedCallableShape {
                        context_parameter_count: property_header.context_parameter_count,
                        context_value_count: property_header.context_value_count,
                        extension_receiver: property_header.extension_receiver,
                    },
                    index
                        .declaration_header(declaration)
                        .is_some_and(|header| header.flags.has(DeclarationFlags::INLINE)),
                );
                let property = match active {
                    Some(active) => active.property(file, property_declaration),
                    None => index
                        .declaration_range(property_declaration)
                        .and_then(|range| {
                            owner
                                .body_props
                                .iter()
                                .find(|candidate| candidate.span == range)
                        }),
                };
                let mut parameter_names = property
                    .into_iter()
                    .flat_map(|property| property.context_params.iter())
                    .map(|parameter| parameter.name.as_str())
                    .collect::<Vec<_>>();
                if is_setter {
                    parameter_names.push("value");
                }
                index.publish_callable_parameters(
                    callable,
                    parameter_names.into_iter().map(|name| {
                        (
                            name,
                            ResolvedValueParameterFlags::new(false, false, false, false),
                        )
                    }),
                );
            }
            DeclarationKind::Classifier
            | DeclarationKind::EnumEntry
            | DeclarationKind::TypeAlias
            | DeclarationKind::Initializer
            | DeclarationKind::Script => {}
        }
    }
    for (declaration, classifier, delegations) in deferred_interface_delegations {
        let delegations = delegations
            .into_iter()
            .map(|(interface, delegate_source)| {
                let resolved = match table {
                    Some(table) => {
                        super::super::interface_delegation::resolve_interface_delegation(
                            table,
                            index,
                            source.raw(),
                            classifier,
                            interface,
                            delegate_source,
                        )
                    }
                    None => super::super::interface_delegation::resolve_streamed_interface_delegation(
                        platform,
                        index,
                        source.raw(),
                        classifier,
                        interface,
                        delegate_source,
                    ),
                };
                crate::trace_compiler!(
                    "signature",
                    "local interface delegation declaration={declaration:?} classifier={classifier} interface={interface:?} source={delegate_source:?} resolved={}",
                    resolved.is_some(),
                );
                resolved
            })
            .collect::<Option<Vec<_>>>();
        match delegations {
            Some(delegations) => {
                index.publish_checked_local_interface_delegations(declaration, delegations)
            }
            None => failed.push(declaration),
        }
    }
    if failed.is_empty() {
        crate::resolve::override_plans::publish_checked_local_override_plans(
            index,
            platform,
            source.raw(),
            &local_classifiers,
        );
        Ok(())
    } else {
        failed.sort_by_key(|declaration| declaration.raw());
        failed.dedup();
        crate::trace_compiler!(
            "signature",
            "local signature declaration inventory={:?}",
            (0..index.declaration_count())
                .map(|raw| {
                    let declaration = crate::fir::DeclarationId::from_raw(raw as u32);
                    (
                        declaration,
                        index.declaration_anchor(declaration),
                        index.declaration_header(declaration),
                    )
                })
                .collect::<Vec<_>>(),
        );
        for declaration in &failed {
            crate::trace_compiler!(
                "signature",
                "local signature publication failed declaration={declaration:?} anchor={:?} header={:?} signature={:?}",
                index.declaration_anchor(*declaration),
                index.declaration_header(*declaration),
                index.signature(*declaration),
            );
        }
        Err(failed)
    }
}
