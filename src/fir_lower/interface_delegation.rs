//! Materialization of resolved `Interface by constructorParameter` declarations.
//!
//! Pass 1 retains the exact forwarding declarations selected from the applied interface hierarchy.
//! This module only materializes those declarations; it performs no lookup, hierarchy walk,
//! overload selection, or source-spelling recovery.

use crate::fir::{
    DeclarationId, DeclarationKind, ResolvedDelegatedCall, ResolvedDelegatedCallTarget,
    ResolvedDelegatedMember, ResolvedInterfaceDelegateSource, ResolvedInterfaceDelegation,
    ResolvedModuleIndex,
};
use crate::ir::{Callee, IrExpr, IrField, IrFile, IrFunction, IrNodeOrigin, IrProperty, IrTypeOp};
use crate::types::Ty;

use super::FirFileLoweringFailure;

/// Whether this class delegates through its own UNDERLYING VALUE rather than through a field of
/// the delegation's own.
///
/// A value class has exactly one field, and `value class IC(val i: I) : I by i` names that very
/// field as the delegate. Kotlin synthesizes no `$$delegate_0` there — the underlying value IS the
/// delegate, and every forwarder reads it — and it could not: a second field is not a shape a value
/// class has. Synthesizing one gave the class two fields, which the native generator refused by
/// name and the JVM emitter turned into a `putfield` of the wrong type on `constructor-impl`
/// (`VerifyError: Bad type on operand stack`).
///
/// The delegate must be the first CONSTRUCTOR PARAMETER: that is the underlying value. A value
/// class delegating to something else — a constructor-body initializer, say — is not this shape and
/// keeps the ordinary field, which will then be refused as it was, rather than being silently
/// pointed at the wrong storage.
fn delegates_through_its_underlying_value(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    delegation: &ResolvedInterfaceDelegation,
) -> bool {
    index
        .declaration_header(declaration)
        .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::VALUE))
        && matches!(
            delegation.source,
            ResolvedInterfaceDelegateSource::ConstructorParameter(0)
        )
}

/// Where the value class's single source parameter sits in its constructor, past whatever prefix
/// captures a local declaration carries.
fn underlying_parameter_index(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    ir: &IrFile,
    class: crate::ir::ClassId,
) -> Result<usize, FirFileLoweringFailure> {
    let source_parameter_count = primary_constructor_parameter_count(index, declaration)?;
    ir.classes[class as usize]
        .ctor_args
        .len()
        .checked_sub(source_parameter_count)
        .filter(|prefix| *prefix < ir.classes[class as usize].ctor_args.len())
        .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))
}

pub(super) fn predeclare_interface_delegation_fields(
    index: &ResolvedModuleIndex,
    source: crate::fir::SourceFileId,
    inline_payload_declarations: &std::collections::HashSet<DeclarationId>,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(raw as u32);
        let Some(anchor) = index.declaration_anchor(declaration) else {
            continue;
        };
        if (anchor.source != source && !inline_payload_declarations.contains(&declaration))
            || anchor.kind != DeclarationKind::Classifier
        {
            continue;
        }
        let Some(header) = index.classifier_header(declaration) else {
            continue;
        };
        let Some(class) = ir.checked_classifier_classes.get(&declaration).copied() else {
            continue;
        };
        for (ordinal, delegation) in header.interface_delegations.iter().enumerate() {
            let ordinal = u32::try_from(ordinal)
                .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
            if ir
                .checked_interface_delegation_fields
                .contains_key(&(declaration, ordinal))
            {
                continue;
            }
            if delegates_through_its_underlying_value(index, declaration, delegation) {
                // Nothing is pushed: the class's own underlying field is the delegate. WHICH field
                // that is cannot be said here — the property's own field does not exist yet — so
                // the edge is recorded where the constructor's field indices are known, in
                // `materialize_delegation`.
                continue;
            }
            let field = u32::try_from(ir.classes[class as usize].fields.len())
                .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
            ir.classes[class as usize].fields.push(
                IrField::new(format!("$$delegate_{ordinal}"), delegation.interface.get())
                    .with_is_final(true)
                    .with_is_private(true),
            );
            ir.checked_interface_delegation_fields
                .insert((declaration, ordinal), field);
        }
    }
    Ok(())
}

pub(super) fn finalize_interface_delegations(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(raw as u32);
        let Some(anchor) = index.declaration_anchor(declaration) else {
            continue;
        };
        if anchor.kind != DeclarationKind::Classifier {
            continue;
        }
        if index.declaration_header(declaration).is_none() {
            // Matched `expect` classifiers keep their source coordinate but are absent from the
            // actualized semantic index and therefore contribute no delegation realization.
            continue;
        }
        let header = index
            .classifier_header(declaration)
            .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
        if header.interface_delegations.is_empty() {
            continue;
        }
        let Some(class) = ir.checked_classifier_classes.get(&declaration).copied() else {
            continue;
        };
        for (ordinal, delegation) in header.interface_delegations.iter().enumerate() {
            materialize_delegation(index, declaration, class, ordinal, delegation, ir)?;
        }
    }
    Ok(())
}

fn materialize_delegation(
    index: &ResolvedModuleIndex,
    declaration: DeclarationId,
    class: crate::ir::ClassId,
    delegation_ordinal: usize,
    delegation: &ResolvedInterfaceDelegation,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    delegation
        .interface
        .get()
        .non_null()
        .obj_internal()
        .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
    let delegation_ordinal = u32::try_from(delegation_ordinal)
        .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
    let field = if delegates_through_its_underlying_value(index, declaration, delegation) {
        // The underlying value IS the delegate. Its field is the one the class's single
        // constructor parameter backs, which the constructor pass has by now decided.
        let parameter_index = underlying_parameter_index(index, declaration, ir, class)?;
        let field = ir.classes[class as usize].ctor_args[parameter_index]
            .field_index
            .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
        ir.checked_interface_delegation_fields
            .insert((declaration, delegation_ordinal), field);
        field
    } else {
        ir.checked_interface_delegation_fields
            .get(&(declaration, delegation_ordinal))
            .copied()
            .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?
    };
    let first_generated = ir.exprs.len();
    match delegation.source {
        ResolvedInterfaceDelegateSource::ConstructorParameter(_)
            if delegates_through_its_underlying_value(index, declaration, delegation) =>
        {
            // Already the class's own field, already written by the underlying value's property:
            // a second initializer would be a second write of the same value.
        }
        ResolvedInterfaceDelegateSource::ConstructorParameter(parameter) => {
            let source_parameter_count = primary_constructor_parameter_count(index, declaration)?;
            let prefix_count = ir.classes[class as usize]
                .ctor_args
                .len()
                .checked_sub(source_parameter_count)
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
            let parameter_index = prefix_count
                .checked_add(parameter as usize)
                .filter(|index| *index < ir.classes[class as usize].ctor_args.len())
                .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
            ir.classes[class as usize].fields[field as usize].ty =
                ir.classes[class as usize].ctor_args[parameter_index].ty;
            prepend_parameter_initializer(ir, class, field, parameter_index)?;
        }
        ResolvedInterfaceDelegateSource::SyntheticConstructorParameter(parameter) => {
            let parameter_index = parameter as usize;
            if parameter_index >= ir.classes[class as usize].constructor_prefix_count as usize
                || parameter_index >= ir.classes[class as usize].ctor_args.len()
            {
                return Err(FirFileLoweringFailure::MissingClassifier(declaration));
            }
            ir.classes[class as usize].fields[field as usize].ty =
                ir.classes[class as usize].ctor_args[parameter_index].ty;
            prepend_parameter_initializer(ir, class, field, parameter_index)?;
        }
        ResolvedInterfaceDelegateSource::ConstructorBodyInitializer => {
            if !ir
                .checked_interface_delegation_initializers
                .contains(&(declaration, delegation_ordinal))
            {
                return Err(FirFileLoweringFailure::MissingClassifier(declaration));
            }
        }
    }

    for member in &delegation.members {
        match member {
            ResolvedDelegatedMember::Function(member) => {
                let name = member.name.to_string();
                let params = member
                    .call
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                let delegate = delegate_field_read(ir, class, field);
                let result = delegated_call(ir, &member.call, delegate)?;
                let function = add_forwarder(
                    ir,
                    class,
                    name,
                    params.clone(),
                    member.call.result.get(),
                    result,
                );
                if !member.type_parameters.is_empty() {
                    ir.signatures.insert(
                        function,
                        crate::ir::IrGenericSig {
                            type_params: member
                                .type_parameters
                                .iter()
                                .map(|parameter| crate::ir::IrTypeParameter {
                                    name: parameter.name.to_string(),
                                    semantic_name: parameter.semantic_name.to_string(),
                                    bounds: parameter
                                        .bounds
                                        .iter()
                                        .map(|bound| (bound.get(), false))
                                        .collect(),
                                    variance: crate::types::TypeVariance::Invariant,
                                    reified: false,
                                })
                                .collect(),
                            params,
                            ret: Some(member.call.result.get()),
                            supers: Vec::new(),
                        },
                    );
                }
                if let Some(receiver) = member.call.extension_receiver_parameter {
                    ir.extension_receiver_fns.insert(function);
                    ir.fn_context_counts.insert(function, receiver as usize);
                }
                let implementation_owner = ir.classes[class as usize].fq_name;
                ir.function_overrides
                    .entry(implementation_owner)
                    .or_default()
                    .push(crate::ir::IrFunctionOverride {
                        // The forwarder semantically realizes this exact interface declaration;
                        // `implementation_function` names its generated common-IR body.
                        implementation: member.overridden.target,
                        implementation_function: Some(function),
                        implementation_owner,
                        overridden: member.overridden.target,
                        overridden_owner: member.overridden.owner,
                        overridden_is_interface: member.overridden.interface,
                        name: member.name.to_string(),
                        declared_parameters: member
                            .overridden
                            .parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect(),
                        declared_result: member.overridden.result.get(),
                        applied_parameters: member
                            .call
                            .parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect(),
                        applied_result: member.call.result.get(),
                        implementation_parameters: member
                            .call
                            .parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect(),
                        implementation_parameter_identities: member
                            .overridden
                            .parameter_identities
                            .to_vec(),
                        implementation_result: member.call.result.get(),
                        suspend: member.call.suspend,
                        // A delegation forwarder implements an interface obligation no superclass
                        // declaration of this class overrides.
                        superclass_overrides: Vec::new(),
                        depth: 0,
                    });
                if member.call.suspend {
                    ir.suspend_funs.push(function);
                }
            }
            ResolvedDelegatedMember::Property(property) => {
                let name = property.name.to_string();
                let ty = property.ty.get();
                let context_params = property
                    .context_parameters
                    .iter()
                    .map(|parameter| (parameter.name.to_string(), parameter.ty.get()))
                    .collect::<Vec<_>>();
                let context_types = context_params
                    .iter()
                    .map(|(_, parameter)| *parameter)
                    .collect::<Vec<_>>();
                let delegate = delegate_field_read(ir, class, field);
                let getter_call = delegated_call(ir, &property.getter, delegate)?;
                let getter = add_forwarder(
                    ir,
                    class,
                    crate::names::property_getter_name(&name),
                    context_types.clone(),
                    ty,
                    getter_call,
                );
                let setter = property
                    .setter
                    .as_ref()
                    .map(|setter| {
                        let delegate = delegate_field_read(ir, class, field);
                        delegated_call(ir, setter, delegate).map(|call| {
                            let mut parameters = context_types.clone();
                            parameters.push(ty);
                            add_forwarder(
                                ir,
                                class,
                                crate::names::property_setter_name(&name),
                                parameters,
                                Ty::Unit,
                                call,
                            )
                        })
                    })
                    .transpose()?;
                ir.classes[class as usize].properties.push(IrProperty {
                    name,
                    context_params,
                    source_order: u32::MAX,
                    decl_line: 0,
                    ty,
                    visibility: crate::types::Visibility::Public,
                    annotations: Box::new([]),
                    initializer: None,
                    storage_ty: None,
                    backing_field: None,
                    is_var: property.setter.is_some(),
                    is_open: false,
                    is_private: false,
                    setter_is_private: false,
                    getter: Some(getter),
                    setter,
                    getter_jvm_name: None,
                    setter_jvm_name: None,
                    needs_access_bridge: false,
                });
            }
        }
    }
    stamp_generated(ir, first_generated);
    Ok(())
}

fn prepend_parameter_initializer(
    ir: &mut IrFile,
    class: crate::ir::ClassId,
    field: u32,
    parameter_index: usize,
) -> Result<(), FirFileLoweringFailure> {
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let value = ir.add_expr(IrExpr::GetValue(
        u32::try_from(parameter_index + 1)
            .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?,
    ));
    let store = ir.add_expr(IrExpr::SetField {
        receiver,
        class,
        index: field,
        value,
    });
    prepend_initializer(ir, class, store);
    Ok(())
}

fn delegated_call(
    ir: &mut IrFile,
    call: &ResolvedDelegatedCall,
    receiver: u32,
) -> Result<u32, FirFileLoweringFailure> {
    let semantic_parameters = call
        .parameters
        .iter()
        .map(|parameter| parameter.get())
        .collect::<Vec<_>>();
    let (callee, arguments, physical_result) = match &call.target {
        ResolvedDelegatedCallTarget::Module {
            target: _,
            owner,
            name,
            parameters,
            result,
            interface,
        } => {
            if parameters.len() != semantic_parameters.len() {
                return Err(FirFileLoweringFailure::InvalidDelegatedCallShape {
                    expected: u32::try_from(parameters.len()).unwrap_or(u32::MAX),
                    actual: u32::try_from(semantic_parameters.len()).unwrap_or(u32::MAX),
                });
            }
            let arguments = parameters
                .iter()
                .zip(&semantic_parameters)
                .enumerate()
                .map(|(ordinal, (declared, semantic))| {
                    let value = ir.add_expr(IrExpr::GetValue(ordinal as u32 + 1));
                    (*semantic != declared.get())
                        .then(|| {
                            ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: value,
                                type_operand: declared.get(),
                            })
                        })
                        .unwrap_or(value)
                })
                .collect::<Vec<_>>();
            (
                Callee::Virtual {
                    owner: *owner,
                    name: name.to_string(),
                    descriptor: String::new(),
                    params: Some((
                        parameters.iter().map(|parameter| parameter.get()).collect(),
                        result.get(),
                    )),
                    interface: *interface,
                },
                arguments,
                result.get(),
            )
        }
        ResolvedDelegatedCallTarget::External(target) => (
            Callee::External {
                target: *target,
                default_provider: None,
                params: semantic_parameters.clone(),
                ret: call.result.get(),
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter: None,
            },
            (0..semantic_parameters.len())
                .map(|ordinal| ir.add_expr(IrExpr::GetValue(ordinal as u32 + 1)))
                .collect(),
            call.result.get(),
        ),
    };
    let expression = ir.add_expr(IrExpr::Call {
        callee,
        dispatch_receiver: Some(receiver),
        args: arguments,
    });
    match &call.target {
        ResolvedDelegatedCallTarget::Module { parameters, .. } => {
            ir.call_declared_params.insert(
                expression,
                parameters.iter().map(|parameter| parameter.get()).collect(),
            );
        }
        ResolvedDelegatedCallTarget::External(_) => {
            ir.ext_call_source_receiver
                .insert(expression, call.receiver.get());
            if let Some(declared) = call.declared_result {
                ir.call_declared_ret.insert(expression, declared.get());
            }
        }
    }
    if call.suspend {
        ir.suspend_calls.insert(expression, call.result.get());
    }
    Ok((physical_result != call.result.get())
        .then(|| {
            ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: expression,
                type_operand: call.result.get(),
            })
        })
        .unwrap_or(expression))
}

fn primary_constructor_parameter_count(
    index: &ResolvedModuleIndex,
    classifier: DeclarationId,
) -> Result<usize, FirFileLoweringFailure> {
    for raw in 0..index.declaration_count() {
        let declaration = DeclarationId::from_raw(raw as u32);
        if index.declaration_anchor(declaration).is_some_and(|anchor| {
            anchor.owner == Some(classifier)
                && anchor.kind == DeclarationKind::Constructor
                && anchor.sibling == 0
        }) {
            return index
                .signature(declaration)
                .map(|signature| signature.parameters.len())
                .ok_or(FirFileLoweringFailure::MissingCallable(declaration));
        }
    }
    Err(FirFileLoweringFailure::MissingClassifier(classifier))
}

fn prepend_initializer(ir: &mut IrFile, class: crate::ir::ClassId, store: u32) {
    let previous = ir.classes[class as usize].init_body.take();
    let body = ir.add_expr(IrExpr::Block {
        stmts: std::iter::once(store).chain(previous).collect(),
        value: None,
    });
    ir.classes[class as usize].init_body = Some(body);
}

fn delegate_field_read(ir: &mut IrFile, class: crate::ir::ClassId, field: u32) -> u32 {
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    ir.add_expr(IrExpr::GetField {
        receiver,
        class,
        index: field,
    })
}

fn add_forwarder(
    ir: &mut IrFile,
    class: crate::ir::ClassId,
    name: String,
    params: Vec<Ty>,
    ret: Ty,
    call: u32,
) -> u32 {
    let statement = if ret == Ty::Unit {
        call
    } else {
        ir.add_expr(IrExpr::Return(Some(call)))
    };
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![statement],
        value: None,
    });
    let parameter_count = params.len();
    let function = ir.add_fun(IrFunction {
        name,
        params,
        ret,
        body: Some(body),
        is_static: false,
        dispatch_receiver: Some(ir.classes[class as usize].fq_name),
        param_checks: vec![None; parameter_count],
    });
    ir.classes[class as usize].methods.push(function);
    function
}

fn stamp_generated(ir: &mut IrFile, first: usize) {
    let cause = crate::fir::OriginId::from_raw(0);
    for raw in first..ir.exprs.len() {
        ir.fir_origins
            .entry(raw as u32)
            .or_insert(IrNodeOrigin::Synthetic {
                cause,
                kind: crate::fir::SyntheticOriginKind::GeneratedAccessor,
            });
    }
}
