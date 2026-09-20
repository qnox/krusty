//! Lowering for a DELEGATED property (`val x: T by delegate`).
//!
//! A delegated property is three generated things: a static (or field) holding the delegate, a
//! static holding its `KProperty` reference, and a pair of accessors that call the delegate's
//! `getValue`/`setValue` operators. The operator is resolved by the checker; what this module owns
//! is the handoff — which value reaches which slot, and at what SEMANTIC type.
//!
//! Every value crossing into or out of the operator is adapted here, against the type the checker
//! recorded for the other side and nothing else: the delegate into the operator's receiver slot,
//! each argument into the parameter the callable DECLARES, and the operator's result into the
//! property's own type. Whether such a boundary costs a box, an unbox, a widening, a `checkcast`
//! or no instruction at all is a JVM representation question that the backend answers when it
//! emits the `ImplicitCoercion`; common lowering only states that the two semantic types differ.

use std::collections::HashMap;

use crate::fir::{
    DeclarationFlags, DeclarationId, FirCallTarget, FirCallableReferenceBinding, FirDelegateCall,
    FirDelegateDispatchReceiver, FirPropertyReferenceTarget, ResolvedModuleIndex,
};
use crate::ir::{
    Callee, ExprId, IrCheckedOperation, IrCheckedProperty, IrExpr, IrField, IrFile,
    IrLocalPropertyLayout, IrNodeOrigin, IrProperty, IrStatic,
};
use crate::types::{Ty, TypeName};

use super::generics::declaration_type_parameters;
use super::properties::{
    add_accessor_function, setter_is_private, stamp_generated_property_nodes, AccessorResult,
};
use super::FirFileLoweringFailure;

#[allow(clippy::too_many_arguments)]
pub(super) fn materialize_top_level_delegate(
    source_order: u32,
    property_id: crate::fir::PropertyId,
    mut property: IrCheckedProperty,
    extension_receiver: Option<Ty>,
    context_parameters: Vec<Ty>,
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    realizations: &mut HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
) -> Result<(), FirFileLoweringFailure> {
    if !context_parameters.is_empty() {
        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            property.declaration,
        ));
    }
    let initializer =
        property
            .delegate
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let plan =
        property
            .delegate_plan
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let cause = ir.fir_origins.get(&initializer).map(|origin| match origin {
        IrNodeOrigin::Fir(origin) => *origin,
        IrNodeOrigin::Synthetic { cause, .. } => *cause,
    });
    let first_generated = ir.exprs.len();
    let property_reference = delegated_property_reference(
        ir,
        property_id,
        extension_receiver.is_some(),
        property.flags,
    );
    // Not this lowering's to decide: the checked plan publishes the type resolution selected the
    // conventions against, and the static and every operand built from it carry that one answer.
    let property_reference_ty = plan.property_reference_type.get();
    let property_reference_static = push_delegate_static(
        ir,
        format!("{}$kprop", property.name),
        property_reference_ty,
        property_reference,
        None,
        source_order,
    )?;
    let owner = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
    let storage_initializer = if let Some(provide) = &plan.provide_delegate {
        let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
        delegated_call(
            index,
            ir,
            property.declaration,
            provide,
            initializer,
            vec![
                (owner, Ty::Null),
                (property_reference, property_reference_ty),
            ],
        )?
    } else {
        initializer
    };
    let delegate_static = push_delegate_static(
        ir,
        format!("{}$delegate", property.name),
        plan.storage_type.get(),
        storage_initializer,
        None,
        source_order,
    )?;
    let owner_ty = extension_receiver.unwrap_or(Ty::Null);
    let receiver = ir.add_expr(IrExpr::GetStatic(delegate_static));
    let owner = if extension_receiver.is_some() {
        ir.add_expr(IrExpr::GetValue(0))
    } else {
        ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null))
    };
    let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
    let read = delegated_call(
        index,
        ir,
        property.declaration,
        &plan.get_value,
        receiver,
        vec![
            (owner, owner_ty),
            (property_reference, property_reference_ty),
        ],
    )?;
    let getter = add_accessor_function(
        ir,
        crate::names::property_getter_name(&property.name),
        extension_receiver.into_iter().collect(),
        property.ty,
        read,
        AccessorResult::Delegated(plan.get_value.result.get()),
        true,
        None,
    );
    let setter = plan
        .set_value
        .as_ref()
        .map(|set_value| {
            let receiver = ir.add_expr(IrExpr::GetStatic(delegate_static));
            let owner = if extension_receiver.is_some() {
                ir.add_expr(IrExpr::GetValue(0))
            } else {
                ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null))
            };
            let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
            let value = ir.add_expr(IrExpr::GetValue(u32::from(extension_receiver.is_some())));
            let write = delegated_call(
                index,
                ir,
                property.declaration,
                set_value,
                receiver,
                vec![
                    (owner, owner_ty),
                    (property_reference, property_reference_ty),
                    (value, property.ty),
                ],
            )?;
            Ok(add_accessor_function(
                ir,
                crate::names::property_setter_name(&property.name),
                extension_receiver
                    .into_iter()
                    .chain(std::iter::once(property.ty))
                    .collect(),
                Ty::Unit,
                write,
                AccessorResult::AsDeclared,
                false,
                None,
            ))
        })
        .transpose()?;
    ir.fn_source_order.insert(getter, source_order);
    if let Some(setter) = setter {
        ir.fn_source_order.insert(setter, source_order);
    }
    realizations.insert(
        property_id,
        IrLocalPropertyLayout::TopLevelAccessor {
            getter,
            setter,
            receiver: extension_receiver,
            context_parameters: Vec::new(),
        },
    );
    stamp_generated_property_nodes(ir, first_generated, cause);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn materialize_member_delegate(
    index: &ResolvedModuleIndex,
    source_order: u32,
    property_id: crate::fir::PropertyId,
    mut property: IrCheckedProperty,
    class_id: crate::ir::ClassId,
    context_parameters: Vec<Ty>,
    ir: &mut IrFile,
    realizations: &mut HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
    initialization: &mut HashMap<crate::ir::ClassId, Vec<(u32, ExprId)>>,
) -> Result<(), FirFileLoweringFailure> {
    if !context_parameters.is_empty() {
        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            property.declaration,
        ));
    }
    let initializer =
        property
            .delegate
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let plan =
        property
            .delegate_plan
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let cause = ir.fir_origins.get(&initializer).map(|origin| match origin {
        IrNodeOrigin::Fir(origin) => *origin,
        IrNodeOrigin::Synthetic { cause, .. } => *cause,
    });
    let first_generated = ir.exprs.len();
    let owner_type = ir.classes[class_id as usize].fq_name;
    let delegate_field = u32::try_from(ir.classes[class_id as usize].fields.len())
        .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
    ir.classes[class_id as usize].fields.push(
        IrField::new(
            format!("{}$delegate", property.name),
            plan.storage_type.get(),
        )
        .with_is_final(true)
        .with_is_private(true),
    );
    let property_reference = delegated_property_reference(ir, property_id, true, property.flags);
    // Not this lowering's to decide: the checked plan publishes the type resolution selected the
    // conventions against, and the static and every operand built from it carry that one answer.
    let property_reference_ty = plan.property_reference_type.get();
    let property_reference_static = push_delegate_static(
        ir,
        format!("{}$kprop", property.name),
        property_reference_ty,
        property_reference,
        Some(owner_type),
        source_order,
    )?;
    let this_ref = ir.add_expr(IrExpr::GetValue(0));
    let storage_initializer = if let Some(provide) = &plan.provide_delegate {
        let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
        delegated_call(
            index,
            ir,
            property.declaration,
            provide,
            initializer,
            vec![
                (this_ref, Ty::Obj(owner_type, &[])),
                (property_reference, property_reference_ty),
            ],
        )?
    } else {
        initializer
    };
    let receiver = ir.add_expr(IrExpr::GetValue(0));
    let store = ir.add_expr(IrExpr::SetField {
        receiver,
        class: class_id,
        index: delegate_field,
        value: storage_initializer,
    });
    ir.property_initializer_stores.insert(store);
    initialization.entry(class_id).or_default().push((
        property
            .initialization_order
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?,
        store,
    ));

    let this_ref = ir.add_expr(IrExpr::GetValue(0));
    let receiver = ir.add_expr(IrExpr::GetField {
        receiver: this_ref,
        class: class_id,
        index: delegate_field,
    });
    let this_ref = ir.add_expr(IrExpr::GetValue(0));
    let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
    let read = delegated_call(
        index,
        ir,
        property.declaration,
        &plan.get_value,
        receiver,
        vec![
            (this_ref, Ty::Obj(owner_type, &[])),
            (property_reference, property_reference_ty),
        ],
    )?;
    let getter = add_accessor_function(
        ir,
        crate::names::property_getter_name(&property.name),
        Vec::new(),
        property.ty,
        read,
        AccessorResult::Delegated(plan.get_value.result.get()),
        true,
        Some(owner_type),
    );
    let setter = plan
        .set_value
        .as_ref()
        .map(|set_value| {
            let this_ref = ir.add_expr(IrExpr::GetValue(0));
            let receiver = ir.add_expr(IrExpr::GetField {
                receiver: this_ref,
                class: class_id,
                index: delegate_field,
            });
            let this_ref = ir.add_expr(IrExpr::GetValue(0));
            let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
            let value = ir.add_expr(IrExpr::GetValue(1));
            let write = delegated_call(
                index,
                ir,
                property.declaration,
                set_value,
                receiver,
                vec![
                    (this_ref, Ty::Obj(owner_type, &[])),
                    (property_reference, property_reference_ty),
                    (value, property.ty),
                ],
            )?;
            Ok(add_accessor_function(
                ir,
                crate::names::property_setter_name(&property.name),
                vec![property.ty],
                Ty::Unit,
                write,
                AccessorResult::AsDeclared,
                false,
                Some(owner_type),
            ))
        })
        .transpose()?;
    ir.classes[class_id as usize].methods.push(getter);
    if let Some(setter) = setter {
        ir.classes[class_id as usize].methods.push(setter);
    }
    ir.fn_source_order.insert(getter, source_order);
    if let Some(setter) = setter {
        ir.fn_source_order.insert(setter, source_order);
    }
    let property_index = ir.classes[class_id as usize].properties.len() as u32;
    ir.classes[class_id as usize].properties.push(IrProperty {
        name: property.name.clone(),
        context_params: Vec::new(),
        source_order,
        decl_line: 0,
        ty: property.ty,
        visibility: property.visibility,
        annotations: Box::new([]),
        initializer: None,
        storage_ty: None,
        backing_field: None,
        is_var: property.flags.has(DeclarationFlags::MUTABLE),
        is_open: property.flags.has(DeclarationFlags::OPEN),
        is_private: property.visibility.is_private(),
        setter_is_private: setter_is_private(index, property.declaration),
        getter: Some(getter),
        setter,
        getter_jvm_name: None,
        setter_jvm_name: None,
        needs_access_bridge: false,
    });
    realizations.insert(
        property_id,
        IrLocalPropertyLayout::Member {
            class: class_id,
            owner: owner_type,
            backing_field: None,
            getter: Some(getter),
            setter,
            interface: false,
            name: property.name,
            ty: property.ty,
            mutable: property.flags.has(DeclarationFlags::MUTABLE),
            private: property.visibility.is_private(),
            context_parameters: Vec::new(),
            property: property_index,
        },
    );
    stamp_generated_property_nodes(ir, first_generated, cause);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn materialize_member_extension_delegate(
    index: &ResolvedModuleIndex,
    source_order: u32,
    property_id: crate::fir::PropertyId,
    mut property: IrCheckedProperty,
    class_id: crate::ir::ClassId,
    extension_receiver: Ty,
    context_parameters: Vec<Ty>,
    ir: &mut IrFile,
    realizations: &mut HashMap<crate::fir::PropertyId, IrLocalPropertyLayout>,
    initialization: &mut HashMap<crate::ir::ClassId, Vec<(u32, ExprId)>>,
) -> Result<(), FirFileLoweringFailure> {
    if !context_parameters.is_empty() || ir.classes[class_id as usize].is_interface {
        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
            property.declaration,
        ));
    }
    let initializer =
        property
            .delegate
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let plan =
        property
            .delegate_plan
            .take()
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?;
    let cause = ir.fir_origins.get(&initializer).map(|origin| match origin {
        IrNodeOrigin::Fir(origin) => *origin,
        IrNodeOrigin::Synthetic { cause, .. } => *cause,
    });
    let first_generated = ir.exprs.len();
    let owner = ir.classes[class_id as usize].fq_name;
    let delegate_field = u32::try_from(ir.classes[class_id as usize].fields.len())
        .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
    ir.classes[class_id as usize].fields.push(
        IrField::new(
            format!("{}$delegate", property.name),
            plan.storage_type.get(),
        )
        .with_is_final(true)
        .with_is_private(true),
    );

    // A member extension property has two semantic receivers. Its declaration reference is the
    // unbound KProperty2-like value; the dispatch instance is supplied separately to
    // `provideDelegate`, while accessors supply the extension receiver to getValue/setValue.
    let property_reference = delegated_property_reference(ir, property_id, true, property.flags);
    // Not this lowering's to decide: the checked plan publishes the type resolution selected the
    // conventions against, and the static and every operand built from it carry that one answer.
    let property_reference_ty = plan.property_reference_type.get();
    let property_reference_static = push_delegate_static(
        ir,
        format!("{}$kprop", property.name),
        property_reference_ty,
        property_reference,
        Some(owner),
        source_order,
    )?;
    let storage_initializer = if let Some(provide) = &plan.provide_delegate {
        let dispatch = ir.add_expr(IrExpr::GetValue(0));
        let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
        delegated_call(
            index,
            ir,
            property.declaration,
            provide,
            initializer,
            vec![
                (dispatch, Ty::Obj(owner, &[])),
                (property_reference, property_reference_ty),
            ],
        )?
    } else {
        initializer
    };
    let dispatch = ir.add_expr(IrExpr::GetValue(0));
    let store = ir.add_expr(IrExpr::SetField {
        receiver: dispatch,
        class: class_id,
        index: delegate_field,
        value: storage_initializer,
    });
    ir.property_initializer_stores.insert(store);
    initialization.entry(class_id).or_default().push((
        property
            .initialization_order
            .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(
                property.declaration,
            ))?,
        store,
    ));

    let dispatch = ir.add_expr(IrExpr::GetValue(0));
    let delegate = ir.add_expr(IrExpr::GetField {
        receiver: dispatch,
        class: class_id,
        index: delegate_field,
    });
    let extension = ir.add_expr(IrExpr::GetValue(1));
    let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
    let read = delegated_call(
        index,
        ir,
        property.declaration,
        &plan.get_value,
        delegate,
        vec![
            (extension, extension_receiver),
            (property_reference, property_reference_ty),
        ],
    )?;
    let getter = add_accessor_function(
        ir,
        crate::names::property_getter_name(&property.name),
        vec![extension_receiver],
        property.ty,
        read,
        AccessorResult::Delegated(plan.get_value.result.get()),
        true,
        Some(owner),
    );
    let setter = plan
        .set_value
        .as_ref()
        .map(|set_value| {
            let dispatch = ir.add_expr(IrExpr::GetValue(0));
            let delegate = ir.add_expr(IrExpr::GetField {
                receiver: dispatch,
                class: class_id,
                index: delegate_field,
            });
            let extension = ir.add_expr(IrExpr::GetValue(1));
            let property_reference = ir.add_expr(IrExpr::GetStatic(property_reference_static));
            let value = ir.add_expr(IrExpr::GetValue(2));
            let write = delegated_call(
                index,
                ir,
                property.declaration,
                set_value,
                delegate,
                vec![
                    (extension, extension_receiver),
                    (property_reference, property_reference_ty),
                    (value, property.ty),
                ],
            )?;
            Ok(add_accessor_function(
                ir,
                crate::names::property_setter_name(&property.name),
                vec![extension_receiver, property.ty],
                Ty::Unit,
                write,
                AccessorResult::AsDeclared,
                false,
                Some(owner),
            ))
        })
        .transpose()?;

    let type_params = declaration_type_parameters(index, property.declaration);
    for function in std::iter::once(getter).chain(setter) {
        ir.fn_source_order.insert(function, source_order);
        ir.fresh_method_decls.push(function);
        if property.visibility.is_private() {
            ir.private_methods.insert(function);
        }
        if property.flags.has(DeclarationFlags::OPEN) {
            ir.open_methods.insert(function);
        }
        ir.classes[class_id as usize].methods.push(function);
        if !type_params.is_empty() {
            let signature = &ir.functions[function as usize];
            ir.signatures.insert(
                function,
                crate::ir::IrGenericSig {
                    type_params: type_params.clone(),
                    params: signature.params.clone(),
                    ret: Some(signature.ret),
                    supers: Vec::new(),
                },
            );
        }
    }
    ir.member_ext_props
        .entry(owner)
        .or_default()
        .push(crate::ir::MemberExtProp {
            name: property.name.clone(),
            receiver: extension_receiver,
            ty: property.ty,
            is_var: property.flags.has(DeclarationFlags::MUTABLE),
            is_abstract: false,
            getter,
            setter,
            visibility: property.visibility,
            type_params,
        });
    realizations.insert(
        property_id,
        IrLocalPropertyLayout::MemberExtension {
            owner,
            interface: false,
            name: property.name,
            getter,
            setter,
            receiver: extension_receiver,
            ty: property.ty,
            context_parameters,
        },
    );
    stamp_generated_property_nodes(ir, first_generated, cause);
    Ok(())
}

fn delegated_property_reference(
    ir: &mut IrFile,
    property: crate::fir::PropertyId,
    unbound: bool,
    flags: DeclarationFlags,
) -> ExprId {
    ir.add_expr(IrExpr::Checked(IrCheckedOperation::PropertyReference {
        target: FirPropertyReferenceTarget::Module(property),
        delegated: true,
        binding: if unbound {
            FirCallableReferenceBinding::Unbound
        } else {
            FirCallableReferenceBinding::Static
        },
        dispatch_receiver: None,
        extension_receiver: None,
        mutable: flags.has(DeclarationFlags::MUTABLE),
        substitutions: Vec::new(),
        adaptation: None,
    }))
}

fn push_delegate_static(
    ir: &mut IrFile,
    name: String,
    ty: Ty,
    init: ExprId,
    owner: Option<TypeName>,
    source_order: u32,
) -> Result<u32, FirFileLoweringFailure> {
    let index = u32::try_from(ir.statics.len())
        .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
    ir.statics.push(IrStatic {
        name,
        ty,
        init,
        is_var: false,
        is_const: false,
        owner,
        visibility: crate::types::Visibility::Private,
        setter_jvm_name: None,
        erased_declared_ty: None,
        custom_accessor: true,
        line: 0,
        source_order,
    });
    Ok(index)
}

/// `property` is the DECLARATION this convention call belongs to. Every unsupported shape below
/// reports it: the external arms have no callable identity to name, and `property`
/// is not a spare sentinel — it is whichever declaration was allocated first, so a failure here used
/// to be attributed to an unrelated one.
fn delegated_call(
    index: &ResolvedModuleIndex,
    ir: &mut IrFile,
    property: DeclarationId,
    call: &FirDelegateCall,
    receiver: ExprId,
    arguments: Vec<(ExprId, Ty)>,
) -> Result<ExprId, FirFileLoweringFailure> {
    match &call.target {
        FirCallTarget::Module(target) => {
            let callable =
                index
                    .callable(*target)
                    .ok_or(FirFileLoweringFailure::MissingCallable(
                        DeclarationId::from_raw(target.raw()),
                    ))?;
            let signature = index.signature(callable.declaration).ok_or(
                FirFileLoweringFailure::MissingCallable(callable.declaration),
            )?;
            let arguments =
                materialize_delegated_arguments(ir, arguments, &call.declared_parameters)?;
            let owner = index.enclosing_classifier(callable.declaration);
            if let Some(dispatch) = &call.dispatch_receiver {
                let dispatch = match dispatch {
                    FirDelegateDispatchReceiver::Scoped {
                        current: true,
                        depth: 0,
                        ..
                    } => ir.add_expr(IrExpr::GetValue(0)),
                    FirDelegateDispatchReceiver::Singleton { classifier, .. } => {
                        ir.add_expr(IrExpr::SingletonValue {
                            classifier: *classifier,
                        })
                    }
                    FirDelegateDispatchReceiver::Scoped { .. }
                    | FirDelegateDispatchReceiver::ContextBinding { .. } => {
                        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(
                            callable.declaration,
                        ));
                    }
                };
                let owner = owner.ok_or(FirFileLoweringFailure::MissingCallable(
                    callable.declaration,
                ))?;
                let class = ir
                    .checked_classifier_classes
                    .get(&owner.declaration)
                    .copied()
                    .ok_or(FirFileLoweringFailure::MissingClassifier(owner.declaration))?;
                let function = ir.checked_callable_functions.get(target).copied().ok_or(
                    FirFileLoweringFailure::MissingCallable(callable.declaration),
                )?;
                let method = ir.classes[class as usize]
                    .methods
                    .iter()
                    .position(|candidate| *candidate == function)
                    .ok_or(FirFileLoweringFailure::MissingCallable(
                        callable.declaration,
                    ))? as u32;
                let mut arguments = arguments;
                arguments.insert(callable.shape.context_parameter_count as usize, receiver);
                let expression = ir.add_expr(IrExpr::MethodCall {
                    class,
                    index: method,
                    receiver: dispatch,
                    args: arguments.into_iter().map(Some).collect(),
                });
                return Ok(materialize_delegated_result(
                    ir,
                    expression,
                    signature.result.get(),
                    call.result.get(),
                ));
            }
            if !call.extension {
                if let Some(owner) = owner {
                    if let (Some(class), Some(function)) = (
                        ir.checked_classifier_classes
                            .get(&owner.declaration)
                            .copied(),
                        ir.checked_callable_functions.get(target).copied(),
                    ) {
                        let method = ir.classes[class as usize]
                            .methods
                            .iter()
                            .position(|candidate| *candidate == function)
                            .ok_or(FirFileLoweringFailure::MissingCallable(
                                callable.declaration,
                            ))? as u32;
                        let expression = ir.add_expr(IrExpr::MethodCall {
                            class,
                            index: method,
                            receiver,
                            args: arguments.into_iter().map(Some).collect(),
                        });
                        return Ok(materialize_delegated_result(
                            ir,
                            expression,
                            signature.result.get(),
                            call.result.get(),
                        ));
                    }
                    let expression = ir.add_expr(IrExpr::Call {
                        callee: Callee::Virtual {
                            owner: owner.classifier,
                            name: index
                                .callable_name(*target)
                                .ok_or(FirFileLoweringFailure::MissingCallable(
                                    callable.declaration,
                                ))?
                                .to_owned(),
                            descriptor: String::new(),
                            params: Some((
                                signature.parameters.iter().map(|ty| ty.get()).collect(),
                                signature.result.get(),
                            )),
                            interface: index.declaration_header(owner.declaration).is_some_and(
                                |header| header.flags.has(DeclarationFlags::INTERFACE),
                            ),
                        },
                        dispatch_receiver: Some(receiver),
                        args: arguments,
                    });
                    return Ok(materialize_delegated_result(
                        ir,
                        expression,
                        signature.result.get(),
                        call.result.get(),
                    ));
                }
            }
            let mut arguments = arguments;
            let mut parameters = signature
                .parameters
                .iter()
                .map(|ty| ty.get())
                .collect::<Vec<_>>();
            if call.extension {
                let position = callable.shape.context_parameter_count as usize;
                let extension_receiver = callable.shape.extension_receiver.ok_or(
                    FirFileLoweringFailure::MissingCallable(callable.declaration),
                )?;
                if position > arguments.len() || position > parameters.len() {
                    let expected = u32::try_from(parameters.len() + 1)
                        .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
                    let actual = u32::try_from(arguments.len() + 1)
                        .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
                    return Err(FirFileLoweringFailure::InvalidDelegatedCallShape {
                        expected,
                        actual,
                    });
                }
                // Both sides of the boundary come from the CHECKED call: its own receiver type
                // and the receiver the selected callable declares. Asking the caller for the
                // former left `provideDelegate` with nothing to compare, so a `by 42` receiver
                // reached an erased `Object` slot as an `int`.
                let receiver = materialize_delegated_receiver(
                    ir,
                    receiver,
                    call.receiver.get(),
                    call.declared_receiver
                        .map_or_else(|| extension_receiver.get(), |declared| declared.get()),
                );
                arguments.insert(position, receiver);
                parameters.insert(position, extension_receiver.get());
            }
            let function = ir.checked_callable_functions.get(target).copied();
            let callee = match function {
                Some(function) => Callee::Local(function),
                None => Callee::Module {
                    target: *target,
                    name: index
                        .callable_name(*target)
                        .ok_or(FirFileLoweringFailure::MissingCallable(
                            callable.declaration,
                        ))?
                        .to_owned(),
                    params: parameters,
                    ret: signature.result.get(),
                },
            };
            let expression = ir.add_expr(IrExpr::Call {
                callee,
                dispatch_receiver: None,
                args: arguments,
            });
            Ok(materialize_delegated_result(
                ir,
                expression,
                signature.result.get(),
                call.result.get(),
            ))
        }
        FirCallTarget::External {
            declaration,
            default_provider,
            receiver: _,
            declared_receiver,
            parameters,
            result,
            declared_result,
            suspend,
            inline_plan: _,
            extension_receiver_parameter,
            ..
        } => {
            let mut arguments =
                materialize_delegated_arguments(ir, arguments, &call.declared_parameters)?;
            let dispatch_receiver = if let Some(dispatch) = &call.dispatch_receiver {
                let dispatch = match dispatch {
                    FirDelegateDispatchReceiver::Scoped {
                        current: true,
                        depth: 0,
                        ..
                    } => ir.add_expr(IrExpr::GetValue(0)),
                    FirDelegateDispatchReceiver::Singleton { classifier, .. } => {
                        ir.add_expr(IrExpr::SingletonValue {
                            classifier: *classifier,
                        })
                    }
                    FirDelegateDispatchReceiver::Scoped { .. }
                    | FirDelegateDispatchReceiver::ContextBinding { .. } => {
                        return Err(FirFileLoweringFailure::UnsupportedPropertyShape(property));
                    }
                };
                let parameter = extension_receiver_parameter
                    .ok_or(FirFileLoweringFailure::UnsupportedPropertyShape(property))?
                    as usize;
                if parameter > arguments.len() {
                    return Err(FirFileLoweringFailure::UnsupportedPropertyShape(property));
                }
                arguments.insert(parameter, receiver);
                dispatch
            } else {
                if extension_receiver_parameter.is_some() {
                    return Err(FirFileLoweringFailure::UnsupportedPropertyShape(property));
                }
                receiver
            };
            let expression = ir.add_expr(IrExpr::Call {
                callee: Callee::External {
                    target: *declaration,
                    default_provider: *default_provider,
                    params: parameters.iter().map(|ty| ty.get()).collect(),
                    ret: result.get(),
                    substitutions: Vec::new(),
                    defaults: Vec::new(),
                    extension_receiver_parameter: None,
                },
                dispatch_receiver: Some(dispatch_receiver),
                args: arguments,
            });
            if let Some(declared_receiver) = declared_receiver {
                ir.ext_call_source_receiver
                    .insert(expression, declared_receiver.get());
            }
            if let Some(declared_result) = declared_result {
                ir.call_declared_ret
                    .insert(expression, declared_result.get());
            }
            if *suspend {
                ir.suspend_calls.insert(expression, result.get());
            }
            Ok(expression)
        }
        FirCallTarget::Intrinsic { .. }
        | FirCallTarget::Classifier { .. }
        | FirCallTarget::Super { .. } => {
            Err(FirFileLoweringFailure::UnsupportedPropertyShape(property))
        }
    }
}

/// Preserve the selected call-site result separately from the declaration's erased result slot.
/// This is the same mechanical boundary used by ordinary checked calls: the frontend has already
/// specialized `result`, while the declaration signature remains authoritative for the physical
/// value produced by the call.
/// Adapt a delegate value to the receiver slot the operator DECLARES.
///
/// A delegate is stored at its own type. `val s: String by impl`, where `impl` is an `Int`, reaches
/// an operator whose receiver is `Any?`, and the call pushed a raw `int` where the descriptor says
/// `Object`, which does not verify. The comparison is against the DECLARED slot, so an operator
/// declared on the scalar itself (`operator fun Int.getValue(…)`) states no boundary and the value
/// stays as it is.
fn materialize_delegated_receiver(
    ir: &mut IrFile,
    receiver: ExprId,
    actual: Ty,
    declared: Ty,
) -> ExprId {
    materialize_delegated_slot(ir, receiver, actual, declared)
}

/// Adapt the operator's VALUE arguments to the slots the CHECKED PLAN says they reach.
///
/// This is the same boundary as the receiver, one position over: `var x: Long by pvar` hands the
/// written `long` to `setValue(thisRef, prop, newValue: T)`, whose third slot is the declaration's
/// `T`. The written value is the argument that usually differs, but an extension operator on a
/// scalar owner (`val Int.x by …`) puts one in the `thisRef` slot too, so every argument is
/// compared against its own declared slot.
///
/// The declared slots are [`FirDelegateCall::declared_parameters`] — the slots the CHECKER
/// published for this exact call, un-erased and in the same order as the applied `parameters`
/// beside them, with the extension receiver already taken out. Lowering does not re-derive them
/// from a signature and does not infer which prefix is a context parameter: reading a declaration
/// list back and aligning it by length is a second, silently divergent answer to a question the
/// checker has already answered. A plan whose length disagrees with the arguments the caller built
/// is a broken contract, not something to pass an argument through unadapted, so it fails here
/// rather than emitting a call.
fn materialize_delegated_arguments(
    ir: &mut IrFile,
    arguments: Vec<(ExprId, Ty)>,
    declared: &[crate::fir::ResolvedTy],
) -> Result<Vec<ExprId>, FirFileLoweringFailure> {
    if declared.len() != arguments.len() {
        let expected = u32::try_from(declared.len())
            .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
        let actual = u32::try_from(arguments.len())
            .map_err(|_| FirFileLoweringFailure::ValueIdentityOverflow)?;
        return Err(FirFileLoweringFailure::InvalidDelegatedCallShape { expected, actual });
    }
    Ok(arguments
        .into_iter()
        .zip(declared.iter())
        .map(|((argument, actual), declared)| {
            materialize_delegated_slot(ir, argument, actual, declared.get())
        })
        .collect())
}

/// State that a value crosses into a slot the callable declares at a DIFFERENT semantic type.
/// Shared by the receiver and the value arguments, which cross the same boundary one position
/// apart.
///
/// The comparison is semantic and nothing else: two identical types have no boundary to cross, and
/// what a differing pair costs — a box, an unbox, a widening, a `checkcast`, or no instruction —
/// is read off the physical types by the backend when it emits the coercion. Deciding that here,
/// by asking whether the actual type is a JVM scalar, would put a representation choice in common
/// lowering and would still be the backend's to re-derive.
fn materialize_delegated_slot(ir: &mut IrFile, value: ExprId, actual: Ty, declared: Ty) -> ExprId {
    if actual == declared {
        return value;
    }
    ir.add_expr(IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::ImplicitCoercion,
        arg: value,
        type_operand: declared,
    })
}

fn materialize_delegated_result(
    ir: &mut IrFile,
    expression: ExprId,
    declared: Ty,
    result: Ty,
) -> ExprId {
    if declared == result {
        return expression;
    }
    ir.call_declared_ret.insert(expression, declared);
    ir.physical_types.insert(expression, declared.erased_recv());
    ir.add_expr(IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::ImplicitCoercion,
        arg: expression,
        type_operand: result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fir_lower::tests::{lower_single_source, lower_source_from_set};
    use crate::ir::IrTypeOp;

    #[test]
    fn local_delegate_storage_name_survives_checked_fir_lowering() {
        let ir = lower_single_source(
            "class Delegate { operator fun getValue(owner: Any?, property: Any?): Int = 1 }\n\
             fun read(): Int { val value: Int by Delegate(); return value }\n",
            "LocalDelegateName",
        );
        assert!(
            ir.value_names.values().any(|name| name == "value$delegate"),
            "common IR must retain the checked delegate-storage debug identity: {:?}",
            ir.value_names
        );
    }

    #[test]
    fn generic_member_delegate_result_keeps_its_erased_call_boundary() {
        let ir = lower_single_source(
            "class Delegate<T>(val value: T) {\n\
                 operator fun getValue(owner: Any?, property: Any?): T = value\n\
             }\n\
             class Owner { val number: Int by Delegate(1) }\n",
            "GenericDelegateResult",
        );
        let getter = ir
            .functions
            .iter()
            .find(|function| function.name == "getNumber")
            .expect("generated delegated-property getter");
        let IrExpr::Block { stmts, value: None } =
            ir.expr(getter.body.expect("concrete getter body"))
        else {
            panic!("getter body must be a statement block");
        };
        let [returned] = stmts.as_slice() else {
            panic!("getter body must contain one return");
        };
        let IrExpr::Return(Some(converted)) = ir.expr(*returned) else {
            panic!("getter must return its delegated call");
        };
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand: Ty::Int,
        } = ir.expr(*converted)
        else {
            panic!("generic getValue result must cross an explicit Int coercion");
        };
        assert!(matches!(ir.expr(*call), IrExpr::MethodCall { .. }));
        assert_eq!(
            ir.physical_types.get(call),
            Some(&Ty::obj("kotlin/Any")),
            "the declaration returns its erased T slot before the selected Int result"
        );
    }

    /// A delegated accessor's result crosses EXACTLY ONE coercion, and only when the boundary is real.
    ///
    /// `add_accessor_function` is shared by ordinary, custom and delegated accessors, so the coercion it
    /// adds has to be idempotent in both directions: a generic operator result must be narrowed to the
    /// property's own type once, and an operator that already returns that type must gain nothing. An
    /// earlier revision wrapped unconditionally and produced a coercion on top of a coercion.
    #[test]
    fn a_delegated_accessor_result_crosses_exactly_one_coercion() {
        fn returned_shape(ir: &crate::ir::IrFile, name: &str) -> (bool, Ty) {
            let getter = ir
                .functions
                .iter()
                .find(|function| function.name == name)
                .unwrap_or_else(|| panic!("generated accessor {name}"));
            let IrExpr::Block { stmts, value: None } =
                ir.expr(getter.body.expect("concrete accessor body"))
            else {
                panic!("{name} body must be a statement block");
            };
            let [returned] = stmts.as_slice() else {
                panic!("{name} body must contain one return");
            };
            let IrExpr::Return(Some(value)) = ir.expr(*returned) else {
                panic!("{name} must return its delegated call");
            };
            match ir.expr(*value) {
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg,
                    type_operand,
                } => {
                    assert!(
                        !matches!(
                            ir.expr(*arg),
                            IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                ..
                            }
                        ),
                        "{name}: a coercion on top of a coercion"
                    );
                    (true, *type_operand)
                }
                _ => (false, Ty::Error),
            }
        }

        let ir = lower_single_source(
            "class Generic<T>(val value: T) {\n\
             \x20   operator fun getValue(owner: Any?, property: Any?): T = value\n\
             }\n\
             class Exact {\n\
             \x20   operator fun getValue(owner: Any?, property: Any?): Int = 1\n\
             }\n\
             class Owner {\n\
             \x20   val erased: Int by Generic(1)\n\
             \x20   val exact: Int by Exact()\n\
             }\n",
            "DelegateAccessorCoercion",
        );
        assert_eq!(
            returned_shape(&ir, "getErased"),
            (true, Ty::Int),
            "a generic operator result is narrowed to the property's own type, once"
        );
        assert!(
            !returned_shape(&ir, "getExact").0,
            "an operator that already returns the property's type gains no coercion"
        );
    }

    #[test]
    fn sibling_extension_provide_delegate_keeps_receiver_in_module_call_shape() {
        let ir = lower_source_from_set(
            &[
                (
                    "inline operator fun String.provideDelegate(owner: Any?, property: Any): String = this",
                    "Delegate",
                ),
                (
                    "operator fun String.getValue(owner: Any?, property: Any): String = this\n\
                     val value by \"OK\"",
                    "Consumer",
                ),
            ],
            1,
        );

        let (parameters, arguments) = ir
            .exprs
            .iter()
            .find_map(|expression| match expression {
                IrExpr::Call {
                    callee: Callee::Module { name, params, .. },
                    args,
                    ..
                } if name == "provideDelegate" => Some((params, args)),
                _ => None,
            })
            .expect("sibling provideDelegate call");
        assert_eq!(
            parameters,
            &[
                Ty::String,
                Ty::nullable(Ty::obj("kotlin/Any")),
                Ty::obj("kotlin/Any"),
            ]
        );
        assert_eq!(arguments.len(), parameters.len());
    }

    fn resolved(ty: Ty) -> crate::fir::ResolvedTy {
        crate::fir::ResolvedTy::new(ty).expect("publishable type")
    }

    /// The boundary adapts an argument against the slot the checked plan says it reaches. A plan
    /// whose slot list does not line up with the arguments the caller built is a broken contract
    /// between two phases, and the one thing it must not do is let an argument through unadapted —
    /// that is how a raw `long` reached an `Object` slot and produced a class that does not verify.
    #[test]
    fn a_plan_that_does_not_line_up_is_refused_rather_than_passed_through() {
        let mut ir = IrFile::default();
        let owner = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let property = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Long(1)));
        let property_token = Ty::obj("fixture/PropertyToken");
        let arguments = vec![
            (owner, Ty::Null),
            (property, property_token),
            (value, Ty::Long),
        ];
        let before = ir.exprs.len();

        // One slot short: the shape a context prefix would produce if a context-prefixed operator
        // could be selected as a convention.
        let short = [
            resolved(Ty::nullable(Ty::obj("kotlin/Any"))),
            resolved(Ty::nullable(Ty::obj("kotlin/Any"))),
        ];
        assert!(
            matches!(
                materialize_delegated_arguments(&mut ir, arguments.clone(), &short),
                Err(FirFileLoweringFailure::InvalidDelegatedCallShape {
                    expected: 2,
                    actual: 3
                })
            ),
            "a shorter plan is refused, naming both lengths"
        );
        assert_eq!(
            ir.exprs.len(),
            before,
            "a refused plan adapts nothing and leaves the arena untouched"
        );

        // One slot too many.
        let long = [
            resolved(Ty::nullable(Ty::obj("kotlin/Any"))),
            resolved(Ty::nullable(Ty::obj("kotlin/Any"))),
            resolved(Ty::nullable(Ty::obj("kotlin/Any"))),
            resolved(Ty::nullable(Ty::obj("kotlin/Any"))),
        ];
        assert!(
            matches!(
                materialize_delegated_arguments(&mut ir, arguments.clone(), &long),
                Err(FirFileLoweringFailure::InvalidDelegatedCallShape {
                    expected: 4,
                    actual: 3
                })
            ),
            "a longer plan is refused too"
        );
    }

    /// The matching case, for contrast: each argument is compared against its OWN slot, and only
    /// the one that differs gains a coercion. A rule that adapted by position offset, or that
    /// adapted everything, would not produce this.
    #[test]
    fn a_matching_plan_adapts_exactly_the_slots_that_differ() {
        let mut ir = IrFile::default();
        let owner = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let property = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
        let value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Long(1)));
        let property_token = Ty::obj("fixture/PropertyToken");
        let arguments = vec![
            (owner, Ty::Null),
            (property, property_token),
            (value, Ty::Long),
        ];
        // A setter-shaped three-argument call: only the written value crosses.
        let declared = [
            resolved(Ty::Null),
            resolved(property_token),
            resolved(Ty::nullable(Ty::obj("kotlin/Any"))),
        ];
        let mapped = materialize_delegated_arguments(&mut ir, arguments, &declared)
            .expect("a plan that lines up");
        assert_eq!(mapped[0], owner, "an identical slot is left alone");
        assert_eq!(mapped[1], property, "an identical slot is left alone");
        assert!(
            matches!(
                ir.expr(mapped[2]),
                IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg,
                    type_operand,
                } if *arg == value && *type_operand == Ty::nullable(Ty::obj("kotlin/Any"))
            ),
            "the written value crosses into the slot the operator declares"
        );
    }
}
