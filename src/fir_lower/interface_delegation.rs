//! Materialization of resolved `Interface by constructorParameter` declarations.
//!
//! Pass 1 retains the exact forwarding declarations selected from the applied interface hierarchy.
//! This module only materializes those declarations; it performs no lookup, hierarchy walk,
//! overload selection, or source-spelling recovery.

use crate::fir::{
    DeclarationId, DeclarationKind, ResolvedDelegateCall, ResolvedDelegateMemberCalls,
    ResolvedDelegatedCall, ResolvedDelegatedCallTarget, ResolvedDelegatedMember,
    ResolvedDelegatedModuleTarget, ResolvedFunctionOverrideTarget, ResolvedInterfaceDelegateSource,
    ResolvedInterfaceDelegation, ResolvedModuleIndex,
};
use crate::ir::{
    Callee, IrExpr, IrField, IrFile, IrFunction, IrNodeOrigin, IrProperty, IrTypeOp,
    IrVirtualTarget,
};
use crate::types::Ty;

use super::FirFileLoweringFailure;

/// Move the predeclared delegate-field coordinates when local-class capture storage is inserted
/// ahead of them. The delegation map owns these coordinates; capture lowering only reports the
/// prefix it inserted.
pub(super) fn shift_predeclared_field_indices(
    ir: &mut IrFile,
    declaration: DeclarationId,
    prefix: u32,
) {
    for ((owner, _), field) in &mut ir.checked_interface_delegation_fields {
        if *owner == declaration {
            *field = field
                .checked_add(prefix)
                .expect("captured class field index overflow");
        }
    }
}

/// The physical constructor slot of declared primary-constructor parameter `parameter`. The
/// constructor lowering has laid `ctor_args` out as the compiler prefix (local captures) followed
/// by the declared value parameters, so the slot is read off that recorded layout rather than
/// recomputed from the constructor signature, which also counts classifier context parameters.
fn declared_parameter_index(
    declaration: DeclarationId,
    ir: &IrFile,
    class: crate::ir::ClassId,
    parameter: u32,
) -> Result<usize, FirFileLoweringFailure> {
    let class_ir = &ir.classes[class as usize];
    (class_ir.constructor_prefix_count as usize)
        .checked_add(parameter as usize)
        .filter(|index| *index < class_ir.ctor_args.len())
        .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))
}

pub(super) fn predeclare_interface_delegation_fields(
    index: &ResolvedModuleIndex,
    source: crate::fir::SourceFileId,
    inline_payload_declarations: &std::collections::HashSet<DeclarationId>,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    for declaration in super::declarations_for_lowering(index, source, inline_payload_declarations)
    {
        let Some(anchor) = index.declaration_anchor(declaration) else {
            continue;
        };
        if anchor.kind != DeclarationKind::Classifier {
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
            if matches!(
                delegation.source,
                ResolvedInterfaceDelegateSource::ConstructorProperty(_)
            ) {
                // Nothing is pushed: kotlinc uses a primary-constructor `val` property's own field
                // as the delegate (for a value class it is the only field there may be). WHICH field that is
                // cannot be said here — the property's field does not exist yet — so the edge is
                // recorded where the constructor's field indices are known, in
                // `materialize_delegation`.
                continue;
            }
            let field = u32::try_from(ir.classes[class as usize].fields.len())
                .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
            ir.classes[class as usize].fields.push(
                IrField::new(format!("$$delegate_{ordinal}"), delegation.interface.get())
                    .with_is_final(true)
                    .with_is_private(true)
                    .with_compiler_generated(true),
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
    let mut classifiers = ir
        .checked_classifier_classes
        .keys()
        .copied()
        .collect::<Vec<_>>();
    classifiers.sort_by_key(|declaration| declaration.raw());
    for declaration in classifiers {
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
        // kotlinc stores the delegates in declaration order, ahead of the class's own
        // initializers.
        let mut parameter_stores = Vec::new();
        for (ordinal, delegation) in header.interface_delegations.iter().enumerate() {
            let delegate = DelegationSite {
                declaration,
                class,
                ordinal,
            };
            materialize_delegation(delegate, delegation, &mut parameter_stores, ir)?;
        }
        prepend_initializers(ir, class, parameter_stores);
    }
    Ok(())
}

/// One delegation of a classifier: the classifier, its class, and the delegation's ordinal.
#[derive(Clone, Copy)]
struct DelegationSite {
    declaration: DeclarationId,
    class: crate::ir::ClassId,
    ordinal: usize,
}

fn materialize_delegation(
    site: DelegationSite,
    delegation: &ResolvedInterfaceDelegation,
    parameter_stores: &mut Vec<u32>,
    ir: &mut IrFile,
) -> Result<(), FirFileLoweringFailure> {
    let DelegationSite {
        declaration,
        class,
        ordinal: delegation_ordinal,
    } = site;
    delegation
        .interface
        .get()
        .non_null()
        .obj_internal()
        .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
    let delegation_ordinal = u32::try_from(delegation_ordinal)
        .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
    let field = if let ResolvedInterfaceDelegateSource::ConstructorProperty(parameter) =
        delegation.source
    {
        // The property IS the delegate. Its field is the one its constructor parameter backs,
        // which the constructor pass has by now decided.
        let parameter_index = declared_parameter_index(declaration, ir, class, parameter)?;
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
        ResolvedInterfaceDelegateSource::ConstructorProperty(_) => {
            // Already the class's own field, already written by the property's parameter: a
            // second initializer would be a second write of the same value.
        }
        ResolvedInterfaceDelegateSource::ConstructorParameter(parameter) => {
            let parameter_index = declared_parameter_index(declaration, ir, class, parameter)?;
            ir.classes[class as usize].fields[field as usize].ty =
                ir.classes[class as usize].ctor_args[parameter_index].ty;
            parameter_stores.push(parameter_initializer(ir, class, field, parameter_index)?);
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
            parameter_stores.push(parameter_initializer(ir, class, field, parameter_index)?);
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

    let delegate_calls = ir
        .checked_interface_delegate_calls
        .get(&(declaration, delegation_ordinal))
        .cloned()
        .filter(|calls| calls.len() == delegation.members.len())
        .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
    for (member, delegate_call) in delegation.members.iter().zip(delegate_calls.iter()) {
        match member {
            ResolvedDelegatedMember::Function(member) => {
                let ResolvedDelegateMemberCalls::Function(delegate_call) = delegate_call else {
                    return Err(FirFileLoweringFailure::MissingClassifier(declaration));
                };
                let name = member.name.to_string();
                let params = member
                    .call
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect::<Vec<_>>();
                let delegate = delegate_field_read(ir, class, field);
                let result = delegate_member_call(ir, delegate_call, delegate)?;
                let function = add_forwarder(
                    ir,
                    class,
                    name,
                    params.clone(),
                    member.call.result.get(),
                    result,
                );
                ir.fn_source_names.insert(function, member.name.to_string());
                let parameter_identities = member
                    .parameter_identities
                    .iter()
                    .map(super::resolved_parameter_identity)
                    .collect::<Vec<_>>();
                if parameter_identities.len() != params.len() {
                    return Err(FirFileLoweringFailure::MissingClassifier(declaration));
                }
                ir.fn_params.insert(
                    function,
                    crate::ir::FnParamInfo::identities(parameter_identities),
                );
                if !member.type_parameters.is_empty() {
                    ir.signatures.insert(
                        function,
                        crate::ir::IrGenericSig {
                            type_params: ir_type_parameters(&member.type_parameters),
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
                let implementation = member
                    .overridden
                    .first()
                    .map(|declaration| declaration.target)
                    .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
                let edges = ir
                    .function_overrides
                    .entry(implementation_owner)
                    .or_default();
                for overridden in &member.overridden {
                    edges.push(crate::ir::IrFunctionOverride {
                        // The forwarder semantically realizes this exact interface declaration;
                        // `implementation_function` names its generated common-IR body.
                        implementation,
                        implementation_function: Some(function),
                        implementation_owner,
                        overridden: overridden.target,
                        overridden_owner: overridden.owner,
                        overridden_semantic_role: overridden.semantic_role,
                        collection_barrier: overridden.collection_barrier,
                        overridden_is_interface: overridden.interface,
                        name: member.name.to_string(),
                        declared_parameters: overridden
                            .parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect(),
                        declared_result: overridden.result.get(),
                        applied_parameters: overridden
                            .applied_parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect(),
                        applied_result: overridden.applied_result.get(),
                        implementation_parameters: member
                            .call
                            .parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect(),
                        implementation_parameter_identities: member.parameter_identities.to_vec(),
                        overridden_parameter_identities: overridden.parameter_identities.to_vec(),
                        implementation_result: member.call.result.get(),
                        suspend: member.call.suspend,
                        // A delegation forwarder implements an interface obligation no superclass
                        // declaration of this class overrides.
                        has_kotlin_superclass_override: false,
                        depth: 0,
                    });
                }
                if member.call.suspend {
                    ir.suspend_funs.push(function);
                }
            }
            ResolvedDelegatedMember::Property(property) => {
                let ResolvedDelegateMemberCalls::Property {
                    getter: delegate_getter,
                    setter: delegate_setter,
                } = delegate_call
                else {
                    return Err(FirFileLoweringFailure::MissingClassifier(declaration));
                };
                let name = property.name.to_string();
                let ty = property.ty.get();
                let context_params = property
                    .context_parameters
                    .iter()
                    .map(|parameter| {
                        (
                            parameter.name.to_string(),
                            parameter.kind,
                            parameter.ty.get(),
                        )
                    })
                    .collect::<Vec<_>>();
                let context_types = context_params
                    .iter()
                    .map(|(_, _, parameter)| *parameter)
                    .collect::<Vec<_>>();
                // A member extension's receiver follows its context parameters in every accessor.
                let extension_receiver = property
                    .getter
                    .extension_receiver_parameter
                    .map(|position| {
                        property
                            .getter
                            .parameters
                            .get(position as usize)
                            .map(|receiver| receiver.get())
                            .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))
                    })
                    .transpose()?;
                let mut getter_parameters = context_types.clone();
                getter_parameters.extend(extension_receiver);
                let delegate = delegate_field_read(ir, class, field);
                let getter_call = delegate_member_call(ir, delegate_getter, delegate)?;
                let getter = add_forwarder(
                    ir,
                    class,
                    crate::names::property_getter_name(&name),
                    getter_parameters.clone(),
                    ty,
                    getter_call,
                );
                if property.setter.is_some() != delegate_setter.is_some() {
                    return Err(FirFileLoweringFailure::MissingClassifier(declaration));
                }
                let setter = delegate_setter
                    .as_ref()
                    .map(|setter| {
                        let delegate = delegate_field_read(ir, class, field);
                        delegate_member_call(ir, setter, delegate).map(|call| {
                            let mut parameters = getter_parameters.clone();
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
                // The accessors' parameters, as a source accessor declares them: its context
                // parameters, the extension receiver, then the setter's value.
                let mut identities = context_params
                    .iter()
                    .enumerate()
                    .map(|(ordinal, (name, kind, _))| {
                        context_parameter_identity(ordinal as u32, name, *kind)
                    })
                    .collect::<Option<Vec<_>>>()
                    .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?;
                if extension_receiver.is_some() {
                    identities.push(crate::ir::IrParameterIdentity::extension_receiver());
                    for accessor in std::iter::once(getter).chain(setter) {
                        ir.fn_source_names.insert(accessor, name.clone());
                    }
                }
                ir.fn_params.insert(
                    getter,
                    crate::ir::FnParamInfo::identities(identities.clone()),
                );
                // kotlinc gives an accessor forwarder its receiver and parameters as locals.
                ir.fn_debug_locals
                    .extend(std::iter::once(getter).chain(setter));
                if let Some(setter) = setter {
                    identities.push(crate::ir::IrParameterIdentity::property_setter_value());
                    ir.fn_params
                        .insert(setter, crate::ir::FnParamInfo::identities(identities));
                }
                let implementation_owner = ir.classes[class as usize].fq_name;
                let implementation = property
                    .overridden
                    .first()
                    .ok_or(FirFileLoweringFailure::MissingClassifier(declaration))?
                    .target;
                for overridden in &property.overridden {
                    ir.property_overrides
                        .entry(implementation_owner)
                        .or_default()
                        .push(crate::ir::IrPropertyOverride {
                            // As for a delegated function, the forwarders realize this exact interface
                            // declaration; `implementation_getter` names their generated body. One
                            // generated property can also replace a concrete superclass declaration.
                            implementation,
                            implementation_getter: Some(getter),
                            implementation_setter: setter,
                            implementation_owner,
                            overridden: overridden.target,
                            overridden_owner: overridden.owner,
                            overridden_is_interface: overridden.interface,
                            name: name.clone(),
                            declared_type: overridden.ty.get(),
                            applied_type: overridden.applied_ty.get(),
                            implementation_type: ty,
                            declared_receiver: overridden.receiver.map(crate::fir::ResolvedTy::get),
                            implementation_receiver: extension_receiver,
                            overridden_mutable: overridden.mutable,
                            implementation_mutable: setter.is_some(),
                            has_kotlin_superclass_override: false,
                            depth: overridden.depth,
                        });
                }
                let type_params = ir_type_parameters(&property.type_parameters);
                if !type_params.is_empty() {
                    for accessor in std::iter::once(getter).chain(setter) {
                        let signature = &ir.functions[accessor as usize];
                        ir.signatures.insert(
                            accessor,
                            crate::ir::IrGenericSig {
                                type_params: type_params.clone(),
                                params: signature.params.clone(),
                                ret: Some(signature.ret),
                                supers: Vec::new(),
                            },
                        );
                    }
                }
                if let Some(receiver) = extension_receiver {
                    // A member extension is not a class property: it has no field and its
                    // accessors take the receiver, so it is recorded with the member extensions.
                    ir.member_ext_props
                        .entry(implementation_owner)
                        .or_default()
                        .push(crate::ir::MemberExtProp {
                            name,
                            source_order: u32::MAX,
                            receiver,
                            ty,
                            is_var: setter.is_some(),
                            is_abstract: false,
                            modifiers: DELEGATION_PROPERTY_MODIFIERS,
                            delegate_field: None,
                            getter,
                            setter,
                            visibility: crate::types::Visibility::Public,
                            type_params,
                        });
                    continue;
                }
                ir.classes[class as usize].properties.push(IrProperty {
                    name,
                    context_params,
                    source_order: u32::MAX,
                    decl_line: 0,
                    ty,
                    type_params,
                    visibility: crate::types::Visibility::Public,
                    return_value_status: Default::default(),
                    annotations: Box::new([]),
                    initializer: None,
                    storage_ty: None,
                    backing_field: None,
                    is_var: property.setter.is_some(),
                    is_open: true,
                    modifiers: DELEGATION_PROPERTY_MODIFIERS,
                    delegate_field: None,
                    has_constant_initializer: false,
                    is_private: false,
                    setter_visibility: crate::types::Visibility::Public,
                    getter: Some(getter),
                    setter,
                    getter_jvm_name: None,
                    setter_jvm_name: None,
                    needs_access_bridge: false,
                    accessor_annotations: Default::default(),
                });
            }
        }
    }
    stamp_generated(ir, first_generated);
    Ok(())
}

fn parameter_initializer(
    ir: &mut IrFile,
    class: crate::ir::ClassId,
    field: u32,
    parameter_index: usize,
) -> Result<u32, FirFileLoweringFailure> {
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
    Ok(store)
}

/// Publish the delegate-side calls a checked constructor body carries for its delegations.
pub(super) fn record_delegate_calls(body: &crate::fir::FirBody, ir: &mut IrFile) {
    for calls in body.interface_delegate_calls() {
        ir.checked_interface_delegate_calls
            .insert((calls.classifier, calls.delegation), calls.members.clone());
    }
}

/// A forwarder's call on the delegate, checked as kotlinc's implicit not-null cast checks it.
fn delegate_member_call(
    ir: &mut IrFile,
    call: &ResolvedDelegateCall,
    receiver: u32,
) -> Result<u32, FirFileLoweringFailure> {
    let result = delegated_call(ir, &call.call, receiver)?;
    Ok(match &call.result_check {
        Some(name) => ir.add_expr(IrExpr::NotNullAssert {
            operand: result,
            check: crate::ir::NullCheck::Named(name.to_string()),
        }),
        None => result,
    })
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
            target,
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
                    if *semantic != declared.get() {
                        ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: value,
                            type_operand: declared.get(),
                        })
                    } else {
                        value
                    }
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
                    module_target: None,
                    target: Some(match *target {
                        ResolvedDelegatedModuleTarget::Function(callable) => {
                            IrVirtualTarget::Function(ResolvedFunctionOverrideTarget::Module(
                                callable,
                            ))
                        }
                        ResolvedDelegatedModuleTarget::PropertyGetter(property) => {
                            IrVirtualTarget::PropertyGetter(
                                crate::fir::ResolvedPropertyOverrideTarget::Module(property),
                            )
                        }
                        ResolvedDelegatedModuleTarget::PropertySetter(property) => {
                            IrVirtualTarget::PropertySetter(
                                crate::fir::ResolvedPropertyOverrideTarget::Module(property),
                            )
                        }
                    }),
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
        ResolvedDelegatedCallTarget::Module {
            parameters, result, ..
        } => {
            ir.call_declared_params.insert(
                expression,
                parameters.iter().map(|parameter| parameter.get()).collect(),
            );
            ir.call_declared_ret.insert(expression, result.get());
        }
        ResolvedDelegatedCallTarget::External(_) => {
            ir.ext_call_source_receiver
                .insert(expression, call.receiver.get());
            // The delegate is read from a field of its own static type, which the call dispatches
            // through as a source call on that value does.
            if let Some(class) = call.receiver.get().non_null().obj_internal() {
                ir.dispatch_classes.insert(expression, class);
            }
            if let Some(declared) = call.declared_result {
                ir.call_declared_ret.insert(expression, declared.get());
            }
        }
    }
    if call.suspend {
        ir.suspend_calls.insert(expression, call.result.get());
    }
    Ok(if physical_result != call.result.get() {
        // The declaration's result read at the forwarder's substitution, as at a source call.
        let coercion = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: expression,
            type_operand: call.result.get(),
        });
        ir.declaration_result_coercions.insert(coercion);
        coercion
    } else {
        expression
    })
}

fn prepend_initializers(ir: &mut IrFile, class: crate::ir::ClassId, stores: Vec<u32>) {
    if stores.is_empty() {
        return;
    }
    // The stores open the existing initializer block rather than nesting it, so the constructor
    // keeps the per-property statements whose source lines it maps.
    let previous = ir.classes[class as usize].init_body.take();
    let previous = match previous.map(|body| ir.expr(body).clone()) {
        Some(IrExpr::Block { stmts, value: None }) => stmts,
        _ => previous.into_iter().collect(),
    };
    let body = ir.add_expr(IrExpr::Block {
        stmts: stores.into_iter().chain(previous).collect(),
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

fn ir_type_parameters(
    parameters: &[crate::fir::ResolvedDelegatedTypeParameter],
) -> Vec<crate::ir::IrTypeParameter> {
    parameters
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
        .collect()
}

fn context_parameter_identity(
    ordinal: u32,
    name: &str,
    kind: crate::types::ContextParameterKind,
) -> Option<crate::ir::IrParameterIdentity> {
    match kind {
        crate::types::ContextParameterKind::Named => {
            Some(crate::ir::IrParameterIdentity::context_value(name))
        }
        crate::types::ContextParameterKind::Anonymous => Some(
            crate::ir::IrParameterIdentity::anonymous_context_parameter(ordinal),
        ),
        crate::types::ContextParameterKind::LegacyReceiver => {
            Some(crate::ir::IrParameterIdentity::context_receiver(ordinal))
        }
        crate::types::ContextParameterKind::None => None,
    }
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
    // A forwarder in a generic class signs the member as that class sees it: `class Impl<D>(b:
    // Base<D>) : Base<D> by b` forwards `foo(TD;)TD;`, not its erasure.
    let semantic = params
        .iter()
        .chain(std::iter::once(&ret))
        .any(|ty| crate::types::ty_mentions_any_param(*ty))
        .then(|| (params.clone(), ret));
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
    // A delegated member is an overridable override, whatever the delegating class's modality.
    ir.open_methods.insert(function);
    // kotlinc's forwarder has no source line, yet it names its receiver and parameters.
    ir.fn_debug_locals.insert(function);
    ir.interface_delegation_forwarders.insert(function);
    if let Some(semantic) = semantic {
        ir.member_semantic_sigs.insert(function, semantic);
    }
    function
}

/// A delegated property is an overridable override, whatever the delegating class's modality, and
/// Kotlin metadata records it as a delegation member rather than a declaration.
const DELEGATION_PROPERTY_MODIFIERS: crate::ir::IrPropertyModifiers =
    crate::ir::IrPropertyModifiers {
        modality: crate::ir::IrPropertyModality::Open,
        declared_getter: false,
        declared_setter: false,
        delegated: false,
        lateinit: false,
        member_kind: crate::ir::IrMemberKind::Delegation,
    };

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
