//! Compact header materialization for the temporary legacy signature publisher.

use super::*;
use crate::types::Visibility;

use crate::resolve::ClassNames;

/// One short-lived materialization of a compact callable header for the legacy signature publisher.
/// It contains declaration syntax only; default expressions and the body remain in the transient
/// file until their dedicated passes are migrated. Production builds this from `HeaderSyntaxArena`,
/// making the compact copy authoritative for explicit callable types during the transition.
#[derive(Clone)]
pub(in crate::resolve) struct StreamedCallableHeader {
    pub(in crate::resolve) declaration: crate::fir::DeclarationId,
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) visibility: Visibility,
    pub(in crate::resolve) flags: crate::fir::DeclarationFlags,
    pub(in crate::resolve) signature_inference: Option<crate::fir::InferredSignatureKind>,
    pub(in crate::resolve) receiver: Option<crate::fir::HeaderTypeId>,
    pub(in crate::resolve) receiver_source_spelling: Option<crate::fir::HeaderTypeId>,
    pub(in crate::resolve) parameters: Vec<StreamedCallableParameter>,
    pub(in crate::resolve) result: StreamedResultKind,
    pub(in crate::resolve) explicit_result: Option<crate::fir::HeaderTypeId>,
    pub(in crate::resolve) type_parameters: Vec<String>,
    pub(in crate::resolve) has_reified_type_parameter: bool,
    pub(in crate::resolve) bounds: Vec<(String, crate::fir::HeaderTypeId)>,
    pub(in crate::resolve) context_count: usize,
    pub(in crate::resolve) signature_start: u32,
}

#[derive(Clone)]
pub(in crate::resolve) struct StreamedCallableParameter {
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) ty: crate::fir::HeaderTypeId,
    pub(in crate::resolve) is_vararg: bool,
    pub(in crate::resolve) has_default: bool,
    pub(in crate::resolve) annotations: crate::fir::HeaderTypeRange,
    pub(in crate::resolve) type_annotations: crate::fir::HeaderTypeRange,
    pub(in crate::resolve) annotation_class_literals:
        crate::fir::HeaderParameterAnnotationClassLiteralRange,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(in crate::resolve) enum StreamedResultKind {
    Explicit,
    ImplicitUnit,
    Inferred,
}

pub(in crate::resolve) fn compact_header_type_spelling(
    headers: &crate::fir::StreamedHeaderModule,
    syntax: crate::fir::HeaderTypeId,
) -> Option<String> {
    headers
        .syntax
        .classifier_spelling(syntax, &headers.lookup_names)
}

pub(in crate::resolve) fn collect_compact_header_type_names(
    headers: &crate::fir::StreamedHeaderModule,
    root: crate::fir::HeaderTypeId,
    out: &mut std::collections::HashSet<String>,
) {
    let mut pending = vec![root];
    while let Some(syntax) = pending.pop() {
        let Some(ty) = headers.syntax.ty(syntax) else {
            continue;
        };
        match ty.kind {
            crate::fir::HeaderTypeKind::Classifier {
                detail,
                abbreviated_argument,
            } => {
                let Some(detail) = headers.syntax.classifier_type(detail) else {
                    continue;
                };
                let spelling = headers
                    .syntax
                    .type_path(detail.path)
                    .iter()
                    .map(|segment| headers.lookup_names.get(*segment))
                    .collect::<Option<Vec<_>>>()
                    .map(|segments| segments.join("."));
                if let Some(spelling) = spelling.filter(|spelling| !spelling.is_empty()) {
                    out.insert(spelling);
                }
                pending.extend(
                    headers
                        .syntax
                        .type_operands(detail.arguments)
                        .iter()
                        .copied(),
                );
                pending.extend(abbreviated_argument);
            }
            crate::fir::HeaderTypeKind::Function {
                parameters, result, ..
            } => {
                pending.extend(headers.syntax.type_operands(parameters).iter().copied());
                pending.extend(result);
            }
        }
    }
}

pub(in crate::resolve) fn resolved_compact_annotation_identities(
    headers: &crate::fir::StreamedHeaderModule,
    annotations: crate::fir::HeaderTypeRange,
    class_names: &ClassNames,
) -> Vec<TypeName> {
    headers
        .syntax
        .type_operands(annotations)
        .iter()
        .filter_map(|annotation| compact_header_type_spelling(headers, *annotation))
        .filter_map(|annotation| class_names.classifier_binding(&annotation).ok())
        .collect()
}

pub(in crate::resolve) fn resolved_compact_declaration_annotations(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
    class_names: &ClassNames,
) -> Vec<TypeName> {
    let header = headers
        .syntax
        .declaration(declaration)
        .expect("a projected source declaration must retain compact header syntax");
    resolved_compact_annotation_identities(headers, header.annotations, class_names)
}

pub(in crate::resolve) fn compact_declaration_has_resolved_annotation(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
    class_names: &ClassNames,
    expected: TypeName,
) -> bool {
    resolved_compact_declaration_annotations(headers, declaration, class_names).contains(&expected)
}

pub(in crate::resolve) fn compact_header_has_resolved_annotation(
    headers: &crate::fir::StreamedHeaderModule,
    annotations: crate::fir::HeaderTypeRange,
    class_names: &ClassNames,
    expected: TypeName,
) -> bool {
    resolved_compact_annotation_identities(headers, annotations, class_names).contains(&expected)
}

pub(in crate::resolve) fn compact_header_equality_bound(
    headers: &crate::fir::StreamedHeaderModule,
    parameters: &[StreamedCallableParameter],
    class_names: &ClassNames,
) -> Option<Ty> {
    parameters.iter().find_map(|parameter| {
        let annotations = headers.syntax.type_operands(parameter.annotations);
        headers
            .syntax
            .parameter_annotation_class_literals(parameter.annotation_class_literals)
            .iter()
            .find_map(|argument| {
                let annotation = annotations.get(argument.annotation_ordinal as usize)?;
                let annotation = compact_header_type_spelling(headers, *annotation)?;
                class_names
                    .classifier_binding(&annotation)
                    .ok()
                    .is_some_and(|identity| identity.matches("kotlin/EqualityBound"))
                    .then(|| {
                        compact_header_type_spelling(headers, argument.classifier)
                            .and_then(|classifier| class_names.classifier_binding(&classifier).ok())
                            .map(Ty::obj_name)
                    })
                    .flatten()
            })
    })
}

/// Source-written callable declarations directly owned by one compact classifier, in semantic
/// sibling order. Generated members have no header syntax and are intentionally excluded.
pub(in crate::resolve) fn streamed_owned_callable_declarations(
    headers: &crate::fir::StreamedHeaderModule,
    owner: crate::fir::DeclarationId,
) -> Vec<crate::fir::DeclarationId> {
    let mut declarations = headers
        .stubs
        .iter()
        .filter(|stub| {
            stub.kind == crate::fir::DeclarationKind::Function
                && headers.syntax.declaration(stub.id).is_some()
                && headers
                    .declarations
                    .anchor(stub.id)
                    .is_some_and(|anchor| anchor.owner == Some(owner))
        })
        .filter_map(|stub| {
            headers
                .declarations
                .anchor(stub.id)
                .map(|anchor| (anchor.sibling, stub.id))
        })
        .collect::<Vec<_>>();
    declarations.sort_by_key(|(sibling, _)| *sibling);
    declarations
        .into_iter()
        .map(|(_, declaration)| declaration)
        .collect()
}

/// Materialize a callable header from stable compact identity without consulting a parser
/// declaration or source range.
pub(in crate::resolve) fn streamed_callable_header_by_declaration(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
) -> Option<StreamedCallableHeader> {
    let stub = headers.stub(declaration)?;
    let name = headers.lookup_names.get(stub.lookup_name?)?.to_owned();
    let syntax_declaration = headers.syntax.declaration(declaration)?;
    let crate::fir::HeaderDeclarationKind::Callable {
        receiver,
        parameters,
        result,
        type_parameters,
        bounds,
        context_count,
        signature_start,
        ..
    } = syntax_declaration.kind
    else {
        return None;
    };
    let receiver_source_spelling =
        receiver.and_then(|receiver| headers.syntax.source_spelling(receiver));
    let parameters = headers
        .syntax
        .parameters(parameters)
        .iter()
        .map(|parameter| {
            Some(StreamedCallableParameter {
                name: headers.lookup_names.get(parameter.name)?.to_string(),
                ty: parameter.ty,
                is_vararg: parameter.flags.is_vararg(),
                has_default: parameter.flags.has_default(),
                annotations: parameter.annotations,
                type_annotations: parameter.type_annotations,
                annotation_class_literals: parameter.annotation_class_literals,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let (result, explicit_result) = match result {
        crate::fir::HeaderResultType::Explicit(result) => {
            (StreamedResultKind::Explicit, Some(result))
        }
        crate::fir::HeaderResultType::ImplicitUnit => (StreamedResultKind::ImplicitUnit, None),
        crate::fir::HeaderResultType::Inferred => (StreamedResultKind::Inferred, None),
    };
    let compact_type_parameters = headers.syntax.type_parameters(type_parameters);
    let type_parameters = compact_type_parameters
        .iter()
        .map(|parameter| headers.lookup_names.get(parameter.name).map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let has_reified_type_parameter = compact_type_parameters
        .iter()
        .any(|parameter| parameter.flags.is_reified());
    let bounds = headers
        .syntax
        .bounds(bounds)
        .iter()
        .map(|bound| {
            Some((
                headers.lookup_names.get(bound.parameter)?.to_string(),
                bound.ty,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(StreamedCallableHeader {
        declaration: stub.id,
        name,
        visibility: stub.visibility,
        flags: stub.flags,
        signature_inference: stub.signature_inference,
        receiver,
        receiver_source_spelling,
        parameters,
        result,
        explicit_result,
        type_parameters,
        has_reified_type_parameter,
        bounds,
        context_count: usize::try_from(context_count).ok()?,
        signature_start,
    })
}

pub(in crate::resolve) struct StreamedPropertyHeader {
    pub(in crate::resolve) declaration: crate::fir::DeclarationId,
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) span: Span,
    pub(in crate::resolve) visibility: Visibility,
    pub(in crate::resolve) flags: crate::fir::DeclarationFlags,
    pub(in crate::resolve) signature_inference: Option<crate::fir::InferredSignatureKind>,
    pub(in crate::resolve) getter_declared: bool,
    pub(in crate::resolve) receiver: Option<crate::fir::HeaderTypeId>,
    pub(in crate::resolve) receiver_source_spelling: Option<crate::fir::HeaderTypeId>,
    pub(in crate::resolve) context_parameters: Vec<(String, crate::fir::HeaderTypeId)>,
    pub(in crate::resolve) declared_type: Option<crate::fir::HeaderTypeId>,
    pub(in crate::resolve) backing_field_type: Option<crate::fir::HeaderTypeId>,
    pub(in crate::resolve) type_parameters: Vec<String>,
    pub(in crate::resolve) bounds: Vec<(String, crate::fir::HeaderTypeId)>,
    pub(in crate::resolve) mutable: bool,
    pub(in crate::resolve) setter_visibility: Visibility,
}

/// Materialize a property header from stable compact identity without consulting a parser
/// declaration or source range.
pub(in crate::resolve) fn streamed_property_header_by_declaration(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
) -> Option<StreamedPropertyHeader> {
    let property_stub = headers.stub(declaration)?;
    let setter_visibility = headers
        .stubs
        .iter()
        .find(|stub| {
            stub.kind == crate::fir::DeclarationKind::Accessor
                && headers
                    .declarations
                    .anchor(stub.id)
                    .is_some_and(|anchor| anchor.owner == Some(declaration) && anchor.sibling == 1)
        })
        .map_or(property_stub.visibility, |setter| setter.visibility);
    let getter_declared = headers.owned_stubs(declaration).any(|stub| {
        stub.kind == crate::fir::DeclarationKind::Accessor
            && headers
                .declarations
                .anchor(stub.id)
                .is_some_and(|anchor| anchor.sibling == 0)
    });
    let name = headers
        .lookup_names
        .get(property_stub.lookup_name?)?
        .to_owned();
    let declaration = headers.syntax.declaration(declaration)?;
    let crate::fir::HeaderDeclarationKind::Property {
        receiver,
        context_parameters,
        declared_type,
        getter_type,
        backing_field_type,
        type_parameters,
        bounds,
        mutable,
    } = declaration.kind
    else {
        return None;
    };
    let receiver_source_spelling =
        receiver.and_then(|receiver| headers.syntax.source_spelling(receiver));
    let context_parameters = headers
        .syntax
        .parameters(context_parameters)
        .iter()
        .map(|parameter| {
            Some((
                headers.lookup_names.get(parameter.name)?.to_string(),
                parameter.ty,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let declared_type = declared_type.or(getter_type);
    let type_parameters = headers
        .syntax
        .type_parameters(type_parameters)
        .iter()
        .map(|parameter| headers.lookup_names.get(parameter.name).map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let bounds = headers
        .syntax
        .bounds(bounds)
        .iter()
        .map(|bound| {
            Some((
                headers.lookup_names.get(bound.parameter)?.to_string(),
                bound.ty,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(StreamedPropertyHeader {
        declaration: property_stub.id,
        name,
        span: property_stub.range,
        visibility: property_stub.visibility,
        flags: property_stub.flags,
        signature_inference: property_stub.signature_inference,
        getter_declared,
        receiver,
        receiver_source_spelling,
        context_parameters,
        declared_type,
        backing_field_type,
        type_parameters,
        bounds,
        mutable,
        setter_visibility,
    })
}

#[derive(Clone)]
pub(in crate::resolve) struct StreamedTypeAliasHeader {
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) type_parameters: Vec<String>,
    pub(in crate::resolve) target: crate::fir::HeaderTypeId,
}

/// Project one source's top-level alias declarations from the compact signature graph.
pub(crate) fn streamed_pass_one_file_type_aliases(
    headers: &crate::fir::StreamedHeaderModule,
    source: crate::fir::SourceFileId,
) -> Option<Vec<StreamedTypeAliasHeader>> {
    headers
        .stubs
        .iter()
        .filter(|stub| {
            stub.source == source
                && stub.kind == crate::fir::DeclarationKind::TypeAlias
                && headers
                    .declarations
                    .anchor(stub.id)
                    .is_some_and(|anchor| anchor.owner.is_none())
        })
        .map(|stub| {
            let name = headers.lookup_names.get(stub.lookup_name?)?.to_string();
            let declaration = headers.syntax.declaration(stub.id)?;
            let crate::fir::HeaderDeclarationKind::TypeAlias {
                type_parameters,
                target,
            } = declaration.kind
            else {
                return None;
            };
            let type_parameters = headers
                .syntax
                .type_parameters(type_parameters)
                .iter()
                .map(|parameter| headers.lookup_names.get(parameter.name).map(str::to_string))
                .collect::<Option<Vec<_>>>()?;
            Some(StreamedTypeAliasHeader {
                name,
                type_parameters,
                target,
            })
        })
        .collect()
}

pub(in crate::resolve) fn streamed_type_alias_header_by_declaration(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
) -> Option<StreamedTypeAliasHeader> {
    let stub = headers.stub(declaration)?;
    let name = headers.lookup_names.get(stub.lookup_name?)?.to_owned();
    let header = headers.syntax.declaration(declaration)?;
    let crate::fir::HeaderDeclarationKind::TypeAlias {
        type_parameters,
        target,
    } = header.kind
    else {
        return None;
    };
    let type_parameters = headers
        .syntax
        .type_parameters(type_parameters)
        .iter()
        .map(|parameter| headers.lookup_names.get(parameter.name).map(str::to_owned))
        .collect::<Option<Vec<_>>>()?;
    Some(StreamedTypeAliasHeader {
        name,
        type_parameters,
        target,
    })
}

pub(in crate::resolve) struct StreamedClassifierParameter {
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) ty: crate::fir::HeaderTypeId,
    pub(in crate::resolve) is_vararg: bool,
    pub(in crate::resolve) has_default: bool,
    pub(in crate::resolve) is_property: bool,
    pub(in crate::resolve) is_mutable_property: bool,
    pub(in crate::resolve) visibility: crate::types::Visibility,
    pub(in crate::resolve) is_open: bool,
    pub(in crate::resolve) annotations: crate::fir::HeaderTypeRange,
    pub(in crate::resolve) stable_declaration: Option<crate::fir::DeclarationId>,
}

pub(in crate::resolve) struct StreamedClassifierHeader {
    pub(in crate::resolve) type_parameters: Vec<String>,
    pub(in crate::resolve) lexical_type_parameter_captures: Vec<String>,
    pub(in crate::resolve) type_parameter_variances: Vec<crate::types::TypeVariance>,
    pub(in crate::resolve) bounds: Vec<(String, crate::fir::HeaderTypeId)>,
    pub(in crate::resolve) supertypes: Vec<crate::fir::HeaderTypeId>,
    pub(in crate::resolve) base: Option<crate::fir::HeaderTypeId>,
    pub(in crate::resolve) delegated_interfaces: Vec<usize>,
    pub(in crate::resolve) primary_parameters: Vec<StreamedClassifierParameter>,
}

/// Materialize a classifier header directly from its stable Pass-1 identity.
///
/// Production signature collection consumes the compact inventory after the transient parser
/// declaration has gone away, so this API cannot accept a `ClassDecl` or parser arena identity.
pub(in crate::resolve) fn streamed_classifier_header_by_declaration(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
) -> Option<StreamedClassifierHeader> {
    let declaration = headers.syntax.declaration(declaration)?;
    let crate::fir::HeaderDeclarationKind::Classifier {
        type_parameters,
        lexical_type_parameter_captures,
        bounds,
        supertypes,
        base,
        context_parameters: _,
        primary_parameters,
        delegations,
    } = declaration.kind
    else {
        return None;
    };
    let packed_type_parameters = headers.syntax.type_parameters(type_parameters);
    let type_parameter_names = packed_type_parameters
        .iter()
        .map(|parameter| headers.lookup_names.get(parameter.name).map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let type_parameter_variances = packed_type_parameters
        .iter()
        .map(|parameter| {
            if parameter.flags.is_in() {
                crate::types::TypeVariance::In
            } else if parameter.flags.is_out() {
                crate::types::TypeVariance::Out
            } else {
                crate::types::TypeVariance::Invariant
            }
        })
        .collect();
    let lexical_type_parameter_captures = headers
        .syntax
        .type_parameters(lexical_type_parameter_captures)
        .iter()
        .map(|parameter| headers.lookup_names.get(parameter.name).map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let bounds = headers
        .syntax
        .bounds(bounds)
        .iter()
        .map(|bound| {
            Some((
                headers.lookup_names.get(bound.parameter)?.to_string(),
                bound.ty,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let supertypes = headers.syntax.type_operands(supertypes).to_vec();
    let delegated_interfaces = headers
        .syntax
        .interface_delegations(delegations)
        .iter()
        .map(|delegation| {
            let supertype = delegation.supertype as usize;
            supertypes.get(supertype)?;
            Some(supertype)
        })
        .collect::<Option<Vec<_>>>()?;
    let primary_parameters = headers
        .syntax
        .parameters(primary_parameters)
        .iter()
        .enumerate()
        .map(|(ordinal, parameter)| {
            let property = if parameter.flags.is_property() {
                let ordinal = u32::try_from(ordinal).ok()?;
                Some(headers.owned_stubs(declaration.declaration).find(|stub| {
                    stub.kind == crate::fir::DeclarationKind::Property
                        && headers
                            .declarations
                            .anchor(stub.id)
                            .is_some_and(|anchor| anchor.sibling == ordinal)
                })?)
            } else {
                None
            };
            Some(StreamedClassifierParameter {
                name: headers.lookup_names.get(parameter.name)?.to_string(),
                ty: parameter.ty,
                is_vararg: parameter.flags.is_vararg(),
                has_default: parameter.flags.has_default(),
                is_property: parameter.flags.is_property(),
                is_mutable_property: parameter.flags.is_mutable_property(),
                visibility: property
                    .map_or(crate::types::Visibility::Public, |stub| stub.visibility),
                is_open: property
                    .is_some_and(|stub| stub.flags.has(crate::fir::DeclarationFlags::OPEN)),
                annotations: parameter.annotations,
                stable_declaration: property.map(|stub| stub.id),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(StreamedClassifierHeader {
        type_parameters: type_parameter_names,
        lexical_type_parameter_captures,
        type_parameter_variances,
        bounds,
        supertypes,
        base,
        delegated_interfaces,
        primary_parameters,
    })
}

pub(in crate::resolve) fn streamed_constructor_declaration(
    headers: &crate::fir::StreamedHeaderModule,
    owner: crate::fir::DeclarationId,
    sibling: u32,
) -> Option<crate::fir::DeclarationId> {
    headers
        .stubs
        .iter()
        .find(|stub| {
            stub.kind == crate::fir::DeclarationKind::Constructor
                && headers
                    .declarations
                    .stable_anchor(stub.id)
                    .is_some_and(|anchor| anchor.owner == Some(owner) && anchor.sibling == sibling)
        })
        .map(|stub| stub.id)
}

pub(in crate::resolve) fn streamed_constructor_parameters_by_declaration(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
) -> Option<Vec<StreamedCallableParameter>> {
    let declaration = headers.syntax.declaration(declaration)?;
    let crate::fir::HeaderDeclarationKind::Constructor { parameters, .. } = declaration.kind else {
        return None;
    };
    headers
        .syntax
        .parameters(parameters)
        .iter()
        .map(|parameter| {
            Some(StreamedCallableParameter {
                name: headers.lookup_names.get(parameter.name)?.to_string(),
                ty: parameter.ty,
                is_vararg: parameter.flags.is_vararg(),
                has_default: parameter.flags.has_default(),
                annotations: parameter.annotations,
                type_annotations: parameter.type_annotations,
                annotation_class_literals: parameter.annotation_class_literals,
            })
        })
        .collect()
}
