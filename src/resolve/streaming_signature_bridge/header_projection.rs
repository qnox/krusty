//! Compact header materialization for the temporary legacy signature publisher.

use super::*;
use crate::types::Visibility;

/// One short-lived materialization of a compact callable header for the legacy signature publisher.
/// It contains declaration syntax only; default expressions and the body remain in the transient
/// file until their dedicated passes are migrated. Production builds this from `HeaderSyntaxArena`,
/// making the compact copy authoritative for explicit callable types during the transition.
#[derive(Clone)]
pub(in crate::resolve) struct StreamedCallableHeader {
    pub(in crate::resolve) declaration: Option<crate::fir::DeclarationId>,
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) visibility: Visibility,
    pub(in crate::resolve) flags: crate::fir::DeclarationFlags,
    pub(in crate::resolve) signature_inference: Option<crate::fir::InferredSignatureKind>,
    pub(in crate::resolve) receiver: Option<TypeRef>,
    pub(in crate::resolve) receiver_source_spelling: Option<TypeRef>,
    pub(in crate::resolve) parameters: Vec<StreamedCallableParameter>,
    pub(in crate::resolve) result: StreamedResultKind,
    pub(in crate::resolve) explicit_result: Option<TypeRef>,
    pub(in crate::resolve) type_parameters: Vec<String>,
    pub(in crate::resolve) type_parameter_flags: Vec<crate::fir::HeaderTypeParameterFlags>,
    pub(in crate::resolve) bounds: Vec<(String, TypeRef)>,
    pub(in crate::resolve) context_count: usize,
    pub(in crate::resolve) signature_start: u32,
    pub(in crate::resolve) annotations: Vec<TypeRef>,
}

#[derive(Clone)]
pub(in crate::resolve) struct StreamedCallableParameter {
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) ty: TypeRef,
    pub(in crate::resolve) is_vararg: bool,
    pub(in crate::resolve) has_default: bool,
    pub(in crate::resolve) annotations: Vec<TypeRef>,
    pub(in crate::resolve) type_annotations: Vec<TypeRef>,
    pub(in crate::resolve) annotation_class_literals: Vec<(usize, String)>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(in crate::resolve) enum StreamedResultKind {
    Explicit,
    ImplicitUnit,
    Inferred,
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
    let materialize_range = |range| {
        headers
            .syntax
            .type_operands(range)
            .iter()
            .map(|ty| {
                headers
                    .syntax
                    .transient_type_ref(*ty, &headers.lookup_names)
            })
            .collect::<Option<Vec<_>>>()
    };
    let annotations = materialize_range(syntax_declaration.annotations)?;
    let (receiver, receiver_source_spelling) = match receiver {
        Some(receiver_id) => {
            let receiver = headers
                .syntax
                .transient_type_ref(receiver_id, &headers.lookup_names)?;
            let source_spelling = headers
                .syntax
                .transient_source_spellings(receiver_id, &headers.lookup_names)?;
            let source_spelling = source_spelling.get(&receiver.span).cloned();
            (Some(receiver), source_spelling)
        }
        None => (None, None),
    };
    let parameters = headers
        .syntax
        .parameters(parameters)
        .iter()
        .map(|parameter| {
            Some(StreamedCallableParameter {
                name: headers.lookup_names.get(parameter.name)?.to_string(),
                ty: headers
                    .syntax
                    .transient_type_ref(parameter.ty, &headers.lookup_names)?,
                is_vararg: parameter.flags.is_vararg(),
                has_default: parameter.flags.has_default(),
                annotations: materialize_range(parameter.annotations)?,
                type_annotations: materialize_range(parameter.type_annotations)?,
                annotation_class_literals: headers
                    .syntax
                    .parameter_annotation_class_literals(parameter.annotation_class_literals)
                    .iter()
                    .map(|argument| {
                        let classifier = headers
                            .syntax
                            .type_path(argument.classifier)
                            .iter()
                            .map(|segment| headers.lookup_names.get(*segment))
                            .collect::<Option<Vec<_>>>()?
                            .join(".");
                        Some((
                            usize::try_from(argument.annotation_ordinal).ok()?,
                            classifier,
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let (result, explicit_result) = match result {
        crate::fir::HeaderResultType::Explicit(result) => (
            StreamedResultKind::Explicit,
            Some(
                headers
                    .syntax
                    .transient_type_ref(result, &headers.lookup_names)?,
            ),
        ),
        crate::fir::HeaderResultType::ImplicitUnit => (StreamedResultKind::ImplicitUnit, None),
        crate::fir::HeaderResultType::Inferred => (StreamedResultKind::Inferred, None),
    };
    let compact_type_parameters = headers.syntax.type_parameters(type_parameters);
    let type_parameters = compact_type_parameters
        .iter()
        .map(|parameter| headers.lookup_names.get(parameter.name).map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let type_parameter_flags = compact_type_parameters
        .iter()
        .map(|parameter| parameter.flags)
        .collect();
    let bounds = headers
        .syntax
        .bounds(bounds)
        .iter()
        .map(|bound| {
            Some((
                headers.lookup_names.get(bound.parameter)?.to_string(),
                headers
                    .syntax
                    .transient_type_ref(bound.ty, &headers.lookup_names)?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(StreamedCallableHeader {
        declaration: Some(stub.id),
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
        type_parameter_flags,
        bounds,
        context_count: usize::try_from(context_count).ok()?,
        signature_start,
        annotations,
    })
}

pub(in crate::resolve) fn streamed_callable_signature_span(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
) -> Option<crate::diag::Span> {
    let declaration = headers.syntax.declaration(declaration)?;
    let crate::fir::HeaderDeclarationKind::Callable {
        signature_start,
        signature_end,
        ..
    } = declaration.kind
    else {
        return None;
    };
    Some(crate::diag::Span::new(signature_start, signature_end))
}

pub(in crate::resolve) fn legacy_callable_header(function: &FunDecl) -> StreamedCallableHeader {
    let result = match (&function.ret, &function.body) {
        (Some(_), _) => StreamedResultKind::Explicit,
        (None, FunBody::Expr(_)) => StreamedResultKind::Inferred,
        (None, FunBody::Block(_) | FunBody::None) => StreamedResultKind::ImplicitUnit,
    };
    StreamedCallableHeader {
        declaration: None,
        name: function.name.clone(),
        visibility: function.visibility,
        flags: crate::fir::DeclarationFlags::default()
            .with(crate::fir::DeclarationFlags::INLINE, function.is_inline())
            .with(crate::fir::DeclarationFlags::FINAL, function.is_final())
            .with(crate::fir::DeclarationFlags::OPEN, function.is_open())
            .with(
                crate::fir::DeclarationFlags::OVERRIDE,
                function.is_override(),
            )
            .with(
                crate::fir::DeclarationFlags::ABSTRACT,
                function.is_abstract(),
            )
            .with(crate::fir::DeclarationFlags::SUSPEND, function.is_suspend())
            .with(crate::fir::DeclarationFlags::TAILREC, function.is_tailrec())
            .with(
                crate::fir::DeclarationFlags::OPERATOR,
                function.is_operator(),
            )
            .with(crate::fir::DeclarationFlags::INFIX, function.is_infix())
            .with(
                crate::fir::DeclarationFlags::COMPANION,
                function.is_companion_extension(),
            ),
        signature_inference: match (&function.ret, &function.body) {
            (None, FunBody::Expr(_)) if function.receiver.is_some() => {
                Some(crate::fir::InferredSignatureKind::ExtensionExpression)
            }
            (None, FunBody::Expr(_)) => Some(crate::fir::InferredSignatureKind::ExpressionFunction),
            (Some(_), _) | (None, FunBody::Block(_) | FunBody::None) => None,
        },
        receiver: function.receiver.clone(),
        receiver_source_spelling: None,
        parameters: function
            .params
            .iter()
            .map(|parameter| StreamedCallableParameter {
                name: parameter.name.clone(),
                ty: parameter.ty.clone(),
                is_vararg: parameter.is_vararg,
                has_default: parameter.default.is_some(),
                annotations: parameter
                    .annotations
                    .iter()
                    .map(TypeRef::from_annotation)
                    .collect(),
                type_annotations: Vec::new(),
                annotation_class_literals: Vec::new(),
            })
            .collect(),
        result,
        explicit_result: function.ret.clone(),
        type_parameters: function.type_params.clone(),
        type_parameter_flags: function
            .type_params
            .iter()
            .map(|parameter| {
                crate::fir::HeaderTypeParameterFlags::from_semantics(
                    crate::types::TypeVariance::Invariant,
                    function.non_null_type_params.contains(parameter),
                    function.reified_type_params.contains(parameter),
                )
            })
            .collect(),
        bounds: function.type_param_bounds.clone(),
        context_count: function.context_count,
        signature_start: function.signature_span.lo,
        annotations: function
            .annotations
            .iter()
            .map(TypeRef::from_annotation)
            .collect(),
    }
}

pub(in crate::resolve) struct StreamedPropertyHeader {
    pub(in crate::resolve) declaration: Option<crate::fir::DeclarationId>,
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) span: Span,
    pub(in crate::resolve) visibility: Visibility,
    pub(in crate::resolve) flags: crate::fir::DeclarationFlags,
    pub(in crate::resolve) signature_inference: Option<crate::fir::InferredSignatureKind>,
    pub(in crate::resolve) getter_declared: bool,
    pub(in crate::resolve) receiver: Option<TypeRef>,
    pub(in crate::resolve) receiver_source_spelling: Option<TypeRef>,
    pub(in crate::resolve) context_parameters: Vec<(String, TypeRef)>,
    pub(in crate::resolve) declared_type: Option<TypeRef>,
    pub(in crate::resolve) backing_field_type: Option<TypeRef>,
    pub(in crate::resolve) type_parameters: Vec<String>,
    pub(in crate::resolve) bounds: Vec<(String, TypeRef)>,
    pub(in crate::resolve) mutable: bool,
    pub(in crate::resolve) setter_visibility: Visibility,
    pub(in crate::resolve) annotations: Vec<TypeRef>,
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
    let materialize = |ty| headers.syntax.transient_type_ref(ty, &headers.lookup_names);
    let materialize_range = |range| {
        headers
            .syntax
            .type_operands(range)
            .iter()
            .map(|ty| materialize(*ty))
            .collect::<Option<Vec<_>>>()
    };
    let annotations = materialize_range(declaration.annotations)?;
    let (receiver, receiver_source_spelling) = match receiver {
        Some(receiver_id) => {
            let receiver = materialize(receiver_id)?;
            let spellings = headers
                .syntax
                .transient_source_spellings(receiver_id, &headers.lookup_names)?;
            let source_spelling = spellings.get(&receiver.span).cloned();
            (Some(receiver), source_spelling)
        }
        None => (None, None),
    };
    let context_parameters = headers
        .syntax
        .parameters(context_parameters)
        .iter()
        .map(|parameter| {
            Some((
                headers.lookup_names.get(parameter.name)?.to_string(),
                materialize(parameter.ty)?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let declared_type = match declared_type.or(getter_type) {
        Some(declared_type) => Some(materialize(declared_type)?),
        None => None,
    };
    let backing_field_type = match backing_field_type {
        Some(backing_field_type) => Some(materialize(backing_field_type)?),
        None => None,
    };
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
                materialize(bound.ty)?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(StreamedPropertyHeader {
        declaration: Some(property_stub.id),
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
        annotations,
    })
}

pub(in crate::resolve) fn legacy_property_header(property: &PropDecl) -> StreamedPropertyHeader {
    StreamedPropertyHeader {
        declaration: None,
        name: property.name.clone(),
        span: property.span,
        visibility: property.visibility,
        flags: crate::fir::DeclarationFlags::default()
            .with(crate::fir::DeclarationFlags::EXTERNAL, property.is_external)
            .with(crate::fir::DeclarationFlags::EXPECT, property.is_expect)
            .with(crate::fir::DeclarationFlags::CONST, property.is_const)
            .with(crate::fir::DeclarationFlags::OPEN, property.is_open)
            .with(crate::fir::DeclarationFlags::OVERRIDE, property.is_override)
            .with(crate::fir::DeclarationFlags::ABSTRACT, property.is_abstract)
            .with(crate::fir::DeclarationFlags::MUTABLE, property.is_var)
            .with(crate::fir::DeclarationFlags::LATEINIT, property.is_lateinit)
            .with(
                crate::fir::DeclarationFlags::DELEGATED,
                property.delegate.is_some(),
            )
            .with(
                crate::fir::DeclarationFlags::EXPLICIT_BACKING_FIELD,
                property.explicit_backing_field.is_some(),
            )
            .with(
                crate::fir::DeclarationFlags::CUSTOM_GETTER,
                property.getter.is_some(),
            )
            .with(
                crate::fir::DeclarationFlags::GETTER_READS_BACKING_FIELD,
                property.getter_reads_field,
            )
            .with(
                crate::fir::DeclarationFlags::CUSTOM_SETTER,
                property.setter.is_some(),
            )
            .with(
                crate::fir::DeclarationFlags::SETTER_HAS_BODY,
                property
                    .setter
                    .as_ref()
                    .is_some_and(|setter| setter.body.is_some()),
            )
            .with(
                crate::fir::DeclarationFlags::HAS_INITIALIZER,
                property.init.is_some(),
            )
            .with(
                crate::fir::DeclarationFlags::COMPANION,
                property.is_companion_extension,
            ),
        signature_inference: None,
        getter_declared: property.getter_declared,
        receiver: property.receiver.clone(),
        receiver_source_spelling: None,
        context_parameters: property
            .context_params
            .iter()
            .map(|parameter| (parameter.name.clone(), parameter.ty.clone()))
            .collect(),
        declared_type: property.declared_ty().cloned(),
        backing_field_type: property
            .explicit_backing_field
            .as_ref()
            .and_then(|field| field.ty.clone()),
        type_parameters: property.type_params.clone(),
        bounds: property.type_param_bounds.clone(),
        mutable: property.is_var,
        setter_visibility: property
            .setter
            .as_ref()
            .map_or(property.visibility, |setter| {
                if setter.is_private {
                    Visibility::Private
                } else {
                    property.visibility
                }
            }),
        annotations: property
            .annotations
            .iter()
            .map(TypeRef::from_annotation)
            .collect(),
    }
}

/// Materialize one source's top-level alias declarations while Pass 1 still owns the temporary
/// compact signature graph. Signature collection resolves these shapes into stable semantic alias
/// headers; the graph and these reconstructed `TypeRef`s are destroyed before Pass 2 begins.
pub(crate) fn streamed_pass_one_file_type_aliases(
    headers: &crate::fir::StreamedHeaderModule,
    source: crate::fir::SourceFileId,
) -> Option<Vec<(String, Vec<String>, TypeRef)>> {
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
            let target = headers
                .syntax
                .transient_type_ref(target, &headers.lookup_names)?;
            Some((name, type_parameters, target))
        })
        .collect()
}

/// Materialize one type-alias header by stable declaration identity. This covers nested/local
/// classifier aliases that do not belong to a file-level alias list.
pub(in crate::resolve) fn streamed_type_alias_header_by_declaration(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
) -> Option<(String, Vec<String>, TypeRef)> {
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
    let target = headers
        .syntax
        .transient_type_ref(target, &headers.lookup_names)?;
    Some((name, type_parameters, target))
}

pub(in crate::resolve) struct StreamedClassifierParameter {
    pub(in crate::resolve) name: String,
    pub(in crate::resolve) ty: TypeRef,
    pub(in crate::resolve) is_vararg: bool,
    pub(in crate::resolve) has_default: bool,
    pub(in crate::resolve) is_property: bool,
    pub(in crate::resolve) is_mutable_property: bool,
    pub(in crate::resolve) visibility: crate::types::Visibility,
    pub(in crate::resolve) is_open: bool,
    pub(in crate::resolve) annotations: Vec<TypeRef>,
    pub(in crate::resolve) stable_declaration: Option<crate::fir::DeclarationId>,
}

pub(in crate::resolve) struct StreamedClassifierHeader {
    pub(in crate::resolve) type_parameters: Vec<String>,
    pub(in crate::resolve) lexical_type_parameter_captures: Vec<String>,
    pub(in crate::resolve) type_parameter_variances: Vec<crate::types::TypeVariance>,
    pub(in crate::resolve) bounds: Vec<(String, TypeRef)>,
    pub(in crate::resolve) supertypes: Vec<TypeRef>,
    pub(in crate::resolve) base: Option<TypeRef>,
    pub(in crate::resolve) delegated_interfaces: Vec<TypeRef>,
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
    let materialize = |ty| headers.syntax.transient_type_ref(ty, &headers.lookup_names);
    let bounds = headers
        .syntax
        .bounds(bounds)
        .iter()
        .map(|bound| {
            Some((
                headers.lookup_names.get(bound.parameter)?.to_string(),
                materialize(bound.ty)?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let supertypes = headers
        .syntax
        .type_operands(supertypes)
        .iter()
        .map(|supertype| materialize(*supertype))
        .collect::<Option<Vec<_>>>()?;
    let base = match base {
        Some(base) => Some(materialize(base)?),
        None => None,
    };
    let delegated_interfaces = headers
        .syntax
        .interface_delegations(delegations)
        .iter()
        .map(|delegation| supertypes.get(delegation.supertype as usize).cloned())
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
                ty: materialize(parameter.ty)?,
                is_vararg: parameter.flags.is_vararg(),
                has_default: parameter.flags.has_default(),
                is_property: parameter.flags.is_property(),
                is_mutable_property: parameter.flags.is_mutable_property(),
                visibility: property
                    .map_or(crate::types::Visibility::Public, |stub| stub.visibility),
                is_open: property
                    .is_some_and(|stub| stub.flags.has(crate::fir::DeclarationFlags::OPEN)),
                annotations: headers
                    .syntax
                    .type_operands(parameter.annotations)
                    .iter()
                    .map(|annotation| materialize(*annotation))
                    .collect::<Option<Vec<_>>>()?,
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

pub(in crate::resolve) fn streamed_declaration_annotations(
    headers: &crate::fir::StreamedHeaderModule,
    declaration: crate::fir::DeclarationId,
) -> Option<Vec<TypeRef>> {
    let declaration = headers.syntax.declaration(declaration)?;
    headers
        .syntax
        .type_operands(declaration.annotations)
        .iter()
        .map(|annotation| {
            headers
                .syntax
                .transient_type_ref(*annotation, &headers.lookup_names)
        })
        .collect()
}

pub(in crate::resolve) fn legacy_classifier_header(class: &ClassDecl) -> StreamedClassifierHeader {
    StreamedClassifierHeader {
        type_parameters: class.type_params.clone(),
        lexical_type_parameter_captures: class.lexical_type_parameter_captures.clone(),
        type_parameter_variances: class.type_param_variances.clone(),
        bounds: class.type_param_bounds.clone(),
        supertypes: class.supertypes.clone(),
        base: class.base_class.as_ref().map(|base| TypeRef {
            name: base.clone(),
            flags: crate::ast::TrFlags::default(),
            arg: None,
            targs: class.base_type_args.clone(),
            span: class.base_class_span.unwrap_or(class.span),
            fun_params: Vec::new(),
            fun_context_count: 0,
        }),
        delegated_interfaces: class
            .interface_delegations
            .iter()
            .filter_map(|delegation| {
                class
                    .supertypes
                    .iter()
                    .find(|supertype| supertype.name == delegation.interface)
                    .cloned()
            })
            .collect(),
        primary_parameters: class
            .props
            .iter()
            .map(|parameter| StreamedClassifierParameter {
                name: parameter.name.clone(),
                ty: parameter.ty.clone(),
                is_vararg: parameter.is_vararg,
                has_default: parameter.default.is_some(),
                is_property: parameter.is_property,
                is_mutable_property: parameter.is_var,
                visibility: parameter.visibility,
                is_open: parameter.is_open,
                annotations: parameter
                    .annotations
                    .iter()
                    .map(TypeRef::from_annotation)
                    .collect(),
                stable_declaration: None,
            })
            .collect(),
    }
}

pub(in crate::resolve) fn streamed_secondary_constructor_parameters(
    headers: &crate::fir::StreamedHeaderModule,
    owner: crate::fir::DeclarationId,
    sibling: u32,
) -> Option<Vec<StreamedCallableParameter>> {
    let stub = headers.owned_stubs(owner).find(|stub| {
        stub.kind == crate::fir::DeclarationKind::Constructor
            && headers
                .declarations
                .stable_anchor(stub.id)
                .is_some_and(|anchor| anchor.sibling == sibling)
    })?;
    streamed_constructor_parameters_by_declaration(headers, stub.id)
}

/// Materialize constructor parameters from stable compact identity.
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
                ty: headers
                    .syntax
                    .transient_type_ref(parameter.ty, &headers.lookup_names)?,
                is_vararg: parameter.flags.is_vararg(),
                has_default: parameter.flags.has_default(),
                annotations: headers
                    .syntax
                    .type_operands(parameter.annotations)
                    .iter()
                    .map(|ty| {
                        headers
                            .syntax
                            .transient_type_ref(*ty, &headers.lookup_names)
                    })
                    .collect::<Option<Vec<_>>>()?,
                type_annotations: headers
                    .syntax
                    .type_operands(parameter.type_annotations)
                    .iter()
                    .map(|ty| {
                        headers
                            .syntax
                            .transient_type_ref(*ty, &headers.lookup_names)
                    })
                    .collect::<Option<Vec<_>>>()?,
                annotation_class_literals: headers
                    .syntax
                    .parameter_annotation_class_literals(parameter.annotation_class_literals)
                    .iter()
                    .map(|argument| {
                        let classifier = headers
                            .syntax
                            .type_path(argument.classifier)
                            .iter()
                            .map(|segment| headers.lookup_names.get(*segment))
                            .collect::<Option<Vec<_>>>()?
                            .join(".");
                        Some((
                            usize::try_from(argument.annotation_ordinal).ok()?,
                            classifier,
                        ))
                    })
                    .collect::<Option<Vec<_>>>()?,
            })
        })
        .collect()
}
