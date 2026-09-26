//! Construction of one class member's semantic signature.
//!
//! A member signature is built from its compact header plus the declaration facts the header does
//! not carry. The legacy variant reads the same shape from parser syntax and disappears with the
//! non-streamed path.

use super::*;

/// Build the lexical signature of a method declared on an enum-entry subclass. The parser has no
/// standalone class declaration for that subclass, so these methods do not pass through ordinary
/// class-member collection. They nevertheless own the same generic callable shape and stable type-
/// parameter identities as every other source method.
#[allow(clippy::too_many_arguments)]
pub(in crate::resolve) fn enum_entry_member_signature(
    file: &File,
    method: &FunDecl,
    ret: Ty,
    classes: &ClassNames,
    enclosing_tparams: &TParams,
    compilation_id: u64,
    source_file: u32,
    source_member: crate::libraries::SourceMember,
    libraries: &dyn SymbolSource,
    diags: &mut DiagSink,
) -> Signature {
    let header = legacy_callable_header(method);
    let method_tparams = enclosing_tparams
        .symbolic_extended_with(&header.type_parameters, &header.bounds, &|name| {
            classes.get(name)
        })
        .alpha_renamed_declaration(
            &header.type_parameters,
            compilation_id,
            source_file,
            method.signature_span.lo,
        );
    let mut signature = legacy_member_signature_from_header(
        file,
        method,
        &header,
        ret,
        classes,
        &method_tparams,
        source_file,
        source_member,
        libraries,
        true,
        diags,
    );
    if !header.type_parameters.is_empty() {
        signature.generic_sig = Some(source_generic_signature_from_header(
            &header,
            classes,
            &method_tparams,
            ret,
            None,
            diags,
        ));
    }
    signature
}

/// The declaration facts a member signature needs that its compact header does not carry: the
/// identities annotation resolution already bound, the source the member was declared in, and the
/// default values its parameters were written with.
pub(in crate::resolve) struct MemberSignatureFacts {
    pub(in crate::resolve) annotations: Vec<TypeName>,
    pub(in crate::resolve) source_file: u32,
    pub(in crate::resolve) source_member: Option<crate::libraries::SourceMember>,
    pub(in crate::resolve) param_default_values: Vec<Option<CtorDefaultValue>>,
}

pub(in crate::resolve) fn member_signature_from_header(
    header: &StreamedCallableHeader,
    facts: MemberSignatureFacts,
    ret: Ty,
    classes: &ClassNames,
    mtp: &TParams,
    diags: &mut DiagSink,
) -> Signature {
    let MemberSignatureFacts {
        annotations,
        source_file,
        source_member,
        param_default_values,
    } = facts;
    let params: Vec<Ty> = header
        .parameters
        .iter()
        .map(|parameter| {
            let ty = ty_of_ref(&parameter.ty, classes, mtp, diags);
            semantic_value_parameter_ty(ty, parameter.is_vararg)
        })
        .collect();
    let lambda_param_types: Vec<Vec<Ty>> = header
        .parameters
        .iter()
        .map(|parameter| {
            if !parameter.ty.fun_params.is_empty() || parameter.ty.name == "<fun>" {
                parameter
                    .ty
                    .fun_params
                    .iter()
                    .map(|r| ty_of_ref(r, classes, mtp, diags))
                    .collect()
            } else {
                Vec::new()
            }
        })
        .collect();
    Signature {
        params,
        ret,
        generic_sig: None,
        projected_return_hazard: has_projected_generic_return_hazard_from_header(header),
        flags: SigFlags::default()
            .with_vararg(
                header
                    .parameters
                    .iter()
                    .any(|parameter| parameter.is_vararg),
            )
            .with_is_inline(header.flags.has(crate::fir::DeclarationFlags::INLINE))
            .with_is_operator(header.flags.has(crate::fir::DeclarationFlags::OPERATOR))
            .with_is_infix(header.flags.has(crate::fir::DeclarationFlags::INFIX))
            .with_is_override(header.flags.has(crate::fir::DeclarationFlags::OVERRIDE))
            .with_is_final(header.flags.has(crate::fir::DeclarationFlags::FINAL))
            .with_is_suspend(header.flags.has(crate::fir::DeclarationFlags::SUSPEND))
            .with_is_abstract(header.flags.has(crate::fir::DeclarationFlags::ABSTRACT)),
        annotations,
        equality_bound: header.parameters.iter().find_map(|parameter| {
            parameter
                .annotation_class_literals
                .iter()
                .find_map(|(ordinal, classifier)| {
                    let annotation = parameter.annotations.get(*ordinal)?;
                    classes
                        .get_class(&annotation.name)
                        .is_some_and(|identity| identity.matches("kotlin/EqualityBound"))
                        .then(|| classes.get_class(classifier).map(Ty::obj_name))
                        .flatten()
                })
        }),
        vararg_index: header
            .parameters
            .iter()
            .position(|parameter| parameter.is_vararg),
        required: header
            .parameters
            .iter()
            .take_while(|parameter| !parameter.has_default)
            .count(),
        param_defaults: header
            .parameters
            .iter()
            .map(|parameter| parameter.has_default)
            .collect(),
        exact_params: header
            .parameters
            .iter()
            .map(|parameter| {
                header_type_has_annotation(
                    &parameter.type_annotations,
                    classes,
                    type_name("kotlin/internal/Exact"),
                )
            })
            .collect(),
        no_infer_params: header
            .parameters
            .iter()
            .map(|parameter| {
                header_type_has_annotation(
                    &parameter.type_annotations,
                    classes,
                    type_name("kotlin/internal/NoInfer"),
                )
            })
            .collect(),
        implicit_integer_coercion: header
            .parameters
            .iter()
            .map(|parameter| {
                header_type_has_annotation(
                    &parameter.annotations,
                    classes,
                    type_name("kotlin/internal/ImplicitIntegerCoercion"),
                )
            })
            .collect(),
        param_default_values,
        param_names: header
            .parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect(),
        lambda_param_types,
        lambda_recv: header
            .parameters
            .iter()
            .map(|parameter| parameter.ty.fun_has_receiver())
            .collect(),
        inline_modifiers: header
            .parameters
            .iter()
            .map(|parameter| parameter.inline_modifier)
            .collect(),
        visibility: header.visibility,
        context_count: header.context_count,
        source_decl: None,
        stable_declaration: header.declaration,
        source_file: Some(source_file),
        source_member,
        source_receiver: None,
        package: String::new(),
        contract: None,
        plugin_expression: None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::resolve) fn legacy_member_signature_from_header(
    file: &File,
    method: &FunDecl,
    header: &StreamedCallableHeader,
    ret: Ty,
    classes: &ClassNames,
    method_tparams: &TParams,
    source_file: u32,
    source_member: crate::libraries::SourceMember,
    libraries: &dyn SymbolSource,
    capture_default_values: bool,
    diags: &mut DiagSink,
) -> Signature {
    assert_eq!(
        header.parameters.len(),
        method.params.len(),
        "legacy materialized header and member parameters must stay aligned",
    );
    let defaults = if capture_default_values {
        method
            .params
            .iter()
            .map(|parameter| {
                parameter
                    .default
                    .and_then(|expr| extract_ctor_default(file, expr, classes, libraries))
            })
            .collect()
    } else {
        vec![None; header.parameters.len()]
    };
    let mut signature = member_signature_from_header(
        header,
        MemberSignatureFacts {
            annotations: resolved_header_annotation_identities(&header.annotations, classes),
            source_file,
            source_member: Some(source_member),
            param_default_values: defaults,
        },
        ret,
        classes,
        method_tparams,
        diags,
    );
    signature.equality_bound = method
        .params
        .iter()
        .find_map(|parameter| declared_equality_bound(file, parameter, classes));
    signature.exact_params = method
        .params
        .iter()
        .map(|parameter| {
            has_type_annotation(
                file,
                parameter.ty.span,
                classes,
                type_name("kotlin/internal/Exact"),
            )
        })
        .collect();
    signature.no_infer_params = method
        .params
        .iter()
        .map(|parameter| {
            has_type_annotation(
                file,
                parameter.ty.span,
                classes,
                type_name("kotlin/internal/NoInfer"),
            )
        })
        .collect();
    signature
}

/// Normalize the explicitly typed callable headers of one source classifier into the same
/// [`Signature`] families consumed by [`ModuleSymbols`](crate::module_symbols::ModuleSymbols).
/// This is the declaration-header half of class signature collection: bodies and inferred returns
/// stay deferred, while overloads, generic shapes, defaults, visibility, and source identities are
/// already authoritative enough for another declaration's initializer to select a call.
pub(in crate::resolve) struct DeclaredMemberHeaderContext<'a> {
    pub(in crate::resolve) file: &'a File,
    pub(in crate::resolve) classes: &'a ClassNames,
    pub(in crate::resolve) class_tparams: &'a TParams,
    pub(in crate::resolve) symbolic_class_tparams: &'a TParams,
    pub(in crate::resolve) class_type_parameters: &'a [String],
    pub(in crate::resolve) compact_headers: Option<&'a crate::fir::StreamedHeaderModule>,
    pub(in crate::resolve) resolved_annotations: &'a HashMap<(u32, u32, u32), TypeName>,
    pub(in crate::resolve) compilation_id: u64,
    pub(in crate::resolve) source_file: u32,
    pub(in crate::resolve) libraries: &'a dyn SymbolSource,
    pub(in crate::resolve) owner_is_interface: bool,
}

pub(in crate::resolve) fn declared_member_callable_headers(
    context: &DeclaredMemberHeaderContext<'_>,
    class: &ClassDecl,
    stable_class: Option<crate::fir::DeclarationId>,
    declaration: DeclId,
    diags: &mut DiagSink,
) -> (MethodMap, HashMap<String, Vec<MemberExtFunSig>>) {
    let DeclaredMemberHeaderContext {
        file,
        classes,
        class_tparams,
        symbolic_class_tparams,
        class_type_parameters,
        compact_headers,
        resolved_annotations,
        compilation_id,
        source_file,
        libraries,
        owner_is_interface,
    } = context;
    let mut methods = MethodMap::new();
    let mut extensions: HashMap<String, Vec<MemberExtFunSig>> = HashMap::new();
    let compact_members = compact_headers.map(|headers| {
        streamed_owned_callable_declarations(
            headers,
            stable_class.expect("a production classifier must have a stable identity"),
        )
    });
    let method_count = compact_members
        .as_ref()
        .map_or_else(|| class.methods.len(), Vec::len);
    for method_index in 0..method_count {
        let method = compact_headers
            .is_none()
            .then(|| &class.methods[method_index]);
        let header = match compact_headers {
            Some(headers) => streamed_callable_header_by_declaration(
                headers,
                compact_members
                    .as_ref()
                    .and_then(|declarations| declarations.get(method_index))
                    .copied()
                    .expect("a production member function must have a stable identity"),
            )
            .expect("a production member function must have a compact header"),
            None => legacy_callable_header(
                method.expect("a legacy member function must have parser syntax"),
            ),
        };
        let Some(return_ref) = header.explicit_result.as_ref() else {
            continue;
        };
        let method_tparams =
            class_tparams.extended_with(&header.type_parameters, &header.bounds, &|name| {
                classes.get(name)
            });
        let ret = ty_of_ref(return_ref, classes, &method_tparams, diags);
        let mut signature = if let Some(headers) = compact_headers {
            member_signature_from_header(
                &header,
                MemberSignatureFacts {
                    annotations: compact_declaration_annotation_identities(
                        headers,
                        header.declaration,
                        resolved_annotations,
                    ),
                    source_file: *source_file,
                    source_member: None,
                    param_default_values: vec![None; header.parameters.len()],
                },
                ret,
                classes,
                &method_tparams,
                diags,
            )
        } else {
            let source_member = crate::libraries::SourceMember::Class {
                file: *source_file,
                owner: declaration.0,
                method: method_index as u32,
            };
            legacy_member_signature_from_header(
                file,
                method.expect("a legacy member function must have parser syntax"),
                &header,
                ret,
                classes,
                &method_tparams,
                *source_file,
                source_member,
                *libraries,
                true,
                diags,
            )
        };
        if *owner_is_interface && method.is_some_and(|method| matches!(method.body, FunBody::None))
        {
            signature.flags = signature.flags.with_is_abstract(true);
        }
        if !header.type_parameters.is_empty() || !class_type_parameters.is_empty() {
            let symbolic_method_tparams = symbolic_class_tparams
                .symbolic_extended_with(&header.type_parameters, &header.bounds, &|name| {
                    classes.get(name)
                })
                .alpha_renamed_declaration(
                    &header.type_parameters,
                    *compilation_id,
                    *source_file,
                    header.signature_start,
                );
            signature.generic_sig = Some(source_generic_signature_from_header(
                &header,
                classes,
                &symbolic_method_tparams,
                signature.ret,
                None,
                diags,
            ));
        }
        if let Some(receiver_ty) = header
            .receiver
            .as_ref()
            .map(|receiver| ty_of_ref(receiver, classes, &method_tparams, diags))
        {
            extensions
                .entry(header.name.clone())
                .or_default()
                .push(MemberExtFunSig {
                    receiver_ty,
                    physical_receiver: receiver_ty,
                    physical_params: signature.params.clone(),
                    signature,
                    physical_name: header.name.clone(),
                    external_identity: None,
                    external_default_provider: None,
                    declared_ret: None,
                    inline_body_plan: None,
                });
        } else {
            methods
                .entry(header.name.clone())
                .or_default()
                .push(signature);
        }
    }
    (methods, extensions)
}
