//! A property reference used as a `KProperty` value, realized as a function value.
//!
//! Common lowering keeps such a reference checked
//! ([`IrCheckedOperation::PropertyReference`]): it names the property, its bound receiver and the
//! function type the reference is a value of, and each target chooses the object it becomes. A
//! target whose every reference object is a function value calls [`realize`], which turns each
//! reference into one: an [`IrCallableReference`] whose adapter reads the property, marked
//! [`IrPropertyFunctionReference::reflective`] so the target also answers the reference's
//! reflective members (`get`, `name`) through it. Every other target leaves the checked node to
//! its own reference objects.
//!
//! The read is the checked operation a source read of the property is:
//! [`IrCheckedOperation::PropertyRead`] for a property of this file, through the receiver its
//! selection declares, and [`IrCheckedOperation::ExternalPropertyRead`] for a dependency one,
//! through its getter's checked target. A shape whose read the reference's one receiver cannot
//! supply, or whose getter each read splices, stays checked, and the target declines it by name.

use crate::fir::{
    FirCallableReferenceBinding, FirPropertyDispatch, FirPropertyReferenceTarget, FirPropertyTarget,
};
use crate::ir::{
    ExprId, IrCallableReference, IrCallableReferenceTarget, IrCheckedOperation,
    IrCheckedSubstitution, IrExpr, IrFile, IrFunction, IrLocalPropertyLayout, IrPropertyDispatch,
    IrPropertyFunctionReference, IrTypeOp,
};
use crate::types::{Ty, TyKind};

/// Realize every source `KProperty` reference of `ir` as a function value.
///
/// A delegated property's `KProperty` argument is the delegate's metadata, not a source value,
/// and stays checked.
///
/// A mutable reference gets its write adapter: another file of the program may write through it,
/// and a producer's unit cannot tell, so the writer cannot depend on this unit's own calls.
pub fn realize(ir: &mut IrFile) {
    for id in 0..ir.exprs.len() as ExprId {
        let IrExpr::Checked(checked) = ir.expr(id) else {
            continue;
        };
        let IrCheckedOperation::PropertyReference {
            target,
            delegated: false,
            binding,
            dispatch_receiver,
            extension_receiver,
            mutable,
            substitutions,
            adaptation: None,
            function_type: Some(function_type),
        } = &**checked
        else {
            continue;
        };
        let bound = match (dispatch_receiver, extension_receiver) {
            (None, None) => None,
            (Some(receiver), None) => Some((*receiver, false)),
            (None, Some(receiver)) => Some((*receiver, true)),
            (Some(_), Some(_)) => continue,
        };
        let reference = Reference {
            target: target.clone(),
            binding: *binding,
            bound,
            mutable: *mutable,
            substitutions: substitutions.clone(),
            function_type: *function_type,
        };
        if let Some(value) = function_value(ir, reference) {
            ir.exprs[id as usize] = value;
        }
    }
}

/// The checked facts of one reference site.
struct Reference {
    target: FirPropertyReferenceTarget,
    binding: FirCallableReferenceBinding,
    /// The bound receiver, and whether it is the property's extension receiver.
    bound: Option<(ExprId, bool)>,
    mutable: bool,
    substitutions: Vec<IrCheckedSubstitution>,
    function_type: Ty,
}

/// How the reference's one receiver reaches the property, and the property's type.
struct ReadShape {
    /// The receiver the reference supplies, at the reference's own type.
    receiver: Option<Ty>,
    extension: bool,
    ty: Ty,
    /// The extension receiver and property type the declaration's accessors are compiled
    /// against, which a specialized reference's receiver crosses into and its result out of.
    declared: Option<(Option<Ty>, Ty)>,
}

/// The function value `reference` is, or `None` for a shape whose read its operands cannot
/// supply.
fn function_value(ir: &mut IrFile, reference: Reference) -> Option<IrExpr> {
    let TyKind::Fun(signature) = reference.function_type.non_null().kind() else {
        return None;
    };
    let shape = read_shape(ir, &reference.target)?;
    // The reference carries one receiver: bound, it is the adapter's capture; unbound, the
    // adapter's one parameter. A receiverless property takes neither.
    let captured = match (reference.binding, shape.receiver, reference.bound) {
        (FirCallableReferenceBinding::Unbound, Some(receiver), None)
            if signature.params.as_slice() == [receiver] =>
        {
            false
        }
        (_, None, None) if signature.params.is_empty() => false,
        (_, Some(_), Some((_, extension)))
            if signature.params.is_empty() && extension == shape.extension =>
        {
            true
        }
        _ => return None,
    };
    let receivers = receivers(ir, &shape);
    let read = read(ir, &reference, &shape, receivers)?;
    let body = ir.callable_reference_adapter_body(read, shape.ty, signature.ret);
    let mut params = Vec::new();
    if captured {
        params.extend(shape.receiver);
    }
    params.extend(signature.params.iter().copied());
    let adapter = add_adapter(ir, params, signature.ret, body, captured);
    let writer = reference
        .mutable
        .then(|| writer(ir, &reference, &shape, captured))
        .flatten();
    Some(IrExpr::CallableReference(Box::new(IrCallableReference {
        target: IrCallableReferenceTarget::Property(Box::new(IrPropertyFunctionReference {
            target: reference.target,
            binding: reference.binding,
            bound_extension_receiver: reference.bound.is_some_and(|(_, extension)| extension),
            mutable: reference.mutable,
            reflective: true,
            writer,
            substitutions: reference.substitutions,
        })),
        adapter,
        captures: Vec::new(),
        bound_receiver: reference.bound.map(|(receiver, _)| receiver),
        bound_contexts: 0,
        function_type: reference.function_type,
        declaration_parameters: shape
            .extension
            .then_some(shape.receiver)
            .flatten()
            .into_iter()
            .collect(),
        declaration_result: shape.ty,
        declaration_suspend: false,
        adaptation: None,
        reflection_owner: None,
    })))
}

/// A private static adapter taking `params` and answering `ret` with `body`, whose leading
/// parameter is the reference's capture when `captured`.
fn add_adapter(
    ir: &mut IrFile,
    params: Vec<Ty>,
    ret: Ty,
    body: ExprId,
    captured: bool,
) -> crate::ir::FunId {
    let adapter = ir.add_fun(IrFunction {
        name: format!("$property_ref_{}", ir.functions.len()),
        params,
        ret: crate::types::stored_value_ty(ret),
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.set_method_visibility(adapter, crate::types::Visibility::Private);
    ir.lambda_own_params_from
        .insert(adapter, u32::from(captured));
    adapter
}

/// The adapter writing the property of a mutable reference: it takes the receiver operand in the
/// read adapter's slot, captured or not, then the written value, and answers `Unit`. `None` for a
/// property whose write this does not realize: a dependency property's.
fn writer(
    ir: &mut IrFile,
    reference: &Reference,
    shape: &ReadShape,
    captured: bool,
) -> Option<crate::ir::FunId> {
    let property = reference.target.module()?;
    let substitutions = match &reference.target {
        FirPropertyReferenceTarget::SpecializedModule { .. } => reference.substitutions.clone(),
        _ => Vec::new(),
    };
    let receivers = receivers(ir, shape);
    let slot = u32::from(shape.receiver.is_some());
    let mut value = ir.add_expr(IrExpr::GetValue(slot));
    // The reference is called through the erased `KMutableProperty.set(Any?)`, so the written value
    // reaches this adapter unchecked. Kotlin/Wasm's own bridge casts it to the declared parameter
    // type before the setter body runs: a value of another class fails here, before the write.
    if shape.ty.obj_internal().is_some() {
        value = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: value,
            type_operand: shape.ty,
        });
    }
    let declared = shape.declared.map_or(shape.ty, |(_, declared)| declared);
    if declared != shape.ty {
        value = ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: value,
            type_operand: declared,
        });
    }
    let write = ir.add_expr(IrExpr::Checked(Box::new(
        IrCheckedOperation::PropertyWrite {
            target: property,
            dispatch_receiver: receivers.dispatch,
            extension_receiver: receivers.extension,
            context_arguments: Vec::new(),
            value,
            substitutions,
        },
    )));
    // As a read does, the write records the type a source write's producer does.
    ir.set_logical_type(write, Ty::Unit);
    let body = ir.callable_reference_adapter_body(write, Ty::Unit, Ty::Unit);
    let params = shape.receiver.into_iter().chain([shape.ty]).collect();
    Some(add_adapter(ir, params, Ty::Unit, body, captured))
}

/// The adapter's receiver operand, its first parameter, in the slot an access takes it in.
fn receivers(ir: &mut IrFile, shape: &ReadShape) -> Receivers {
    let operand = shape.receiver.map(|receiver| {
        let value = ir.add_expr(IrExpr::GetValue(0));
        match shape.declared {
            Some((Some(declared), _)) if shape.extension && declared != receiver => {
                ir.add_expr(IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg: value,
                    type_operand: declared,
                })
            }
            _ => value,
        }
    });
    if shape.extension {
        Receivers {
            dispatch: None,
            extension: operand,
        }
    } else {
        Receivers {
            dispatch: operand,
            extension: None,
        }
    }
}

/// The receiver a read of `target` takes, and the property's type. `None` for a property whose
/// read wants another operand than one receiver, or whose getter each read splices.
fn read_shape(ir: &IrFile, target: &FirPropertyReferenceTarget) -> Option<ReadShape> {
    match target {
        FirPropertyReferenceTarget::Module(property) => {
            let ty = ir.checked_properties.get(property)?.ty;
            let (receiver, extension) = match ir.local_property_layouts.get(property)? {
                IrLocalPropertyLayout::TopLevelStorage {
                    qualifier: None, ..
                } => (None, false),
                IrLocalPropertyLayout::TopLevelAccessor {
                    receiver,
                    context_parameters,
                    ..
                } if context_parameters.is_empty() => (*receiver, receiver.is_some()),
                IrLocalPropertyLayout::Member {
                    owner,
                    context_parameters,
                    ..
                } if context_parameters.is_empty() => (Some(Ty::obj_name(*owner)), false),
                _ => return None,
            };
            Some(ReadShape {
                receiver,
                extension,
                ty,
                declared: None,
            })
        }
        FirPropertyReferenceTarget::SpecializedModule {
            receiver,
            extension_receiver,
            property_type,
            declared_receiver,
            declared_property_type,
            getter_inline_splice: None,
            ..
        } => Some(ReadShape {
            receiver: receiver.map(|receiver| receiver.get()),
            extension: *extension_receiver,
            ty: property_type.get(),
            declared: Some((
                declared_receiver.map(|receiver| receiver.get()),
                declared_property_type.get(),
            )),
        }),
        FirPropertyReferenceTarget::External {
            getter,
            extension_receiver,
            property_type,
            ..
        } => {
            let FirPropertyTarget::External {
                receiver,
                parameters,
                extension_receiver_parameter: None,
                ..
            } = getter.as_ref()
            else {
                return None;
            };
            parameters.is_empty().then(|| ReadShape {
                receiver: receiver.map(|receiver| receiver.get()),
                extension: *extension_receiver,
                ty: property_type.get(),
                declared: None,
            })
        }
        FirPropertyReferenceTarget::SpecializedModule { .. }
        | FirPropertyReferenceTarget::Classifier { .. } => None,
    }
}

/// The adapter's receiver operand, in the slot an access takes it in.
struct Receivers {
    dispatch: Option<ExprId>,
    extension: Option<ExprId>,
}

/// The checked read of the referenced property through the adapter's receiver operand, at the
/// property type the reference reads.
fn read(
    ir: &mut IrFile,
    reference: &Reference,
    shape: &ReadShape,
    receivers: Receivers,
) -> Option<ExprId> {
    let module = match &reference.target {
        FirPropertyReferenceTarget::Module(property) => Some((*property, Vec::new())),
        FirPropertyReferenceTarget::SpecializedModule { property, .. } => {
            Some((*property, reference.substitutions.clone()))
        }
        _ => None,
    };
    if let Some((property, substitutions)) = module {
        let read = ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::PropertyRead {
                target: property,
                dispatch_receiver: receivers.dispatch,
                extension_receiver: receivers.extension,
                context_arguments: Vec::new(),
                substitutions,
            },
        )));
        // A module read names no type of its own; its producer records the declared one, as
        // every source read's does.
        let declared = shape.declared.map_or(shape.ty, |(_, declared)| declared);
        ir.set_logical_type(read, declared);
        return Some(if declared == shape.ty {
            read
        } else {
            ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: read,
                type_operand: shape.ty,
            })
        });
    }
    let FirPropertyReferenceTarget::External { getter, .. } = &reference.target else {
        return None;
    };
    let FirPropertyTarget::External {
        property,
        receiver,
        parameters,
        result,
        dispatch,
        ..
    } = getter.as_ref()
    else {
        return None;
    };
    Some(
        ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::ExternalPropertyRead {
                target: *property,
                dispatch: match dispatch {
                    FirPropertyDispatch::Ordinary => IrPropertyDispatch::Ordinary,
                    FirPropertyDispatch::Super { owner, interface } => IrPropertyDispatch::Super {
                        owner: *owner,
                        interface: *interface,
                    },
                },
                receiver: receivers.dispatch.or(receivers.extension),
                arguments: Vec::new(),
                parameters: parameters.iter().map(|parameter| parameter.get()).collect(),
                result: result.get(),
                substitutions: reference.substitutions.clone(),
                source_receiver: receivers
                    .dispatch
                    .and(*receiver)
                    .map(|receiver| receiver.get()),
            },
        ))),
    )
}
