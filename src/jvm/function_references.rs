//! JVM realization of checked dependency function references.
//!
//! FIR has already selected the declaration and fixed the logical invocation signature. This pass
//! performs one provider-identity lookup to attach JVM owner/name/descriptor facts and synthesizes
//! the runtime `FunctionReferenceImpl` carrier; it does not resolve a source name or select an
//! overload.

use super::classpath::{Classpath, ExternalCallableKind};
use crate::fir::{ExternalCallableId, FirCallableReferenceBinding, FirCallableReferenceTarget};
use crate::ir::{
    FrDispatch, FuncRef, IrBinOp, IrCheckedOperation, IrClass, IrExpr, IrFile, IrFunction,
};
use crate::libraries::MemberRealization;
use crate::types::{type_name, Ty};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FunctionReferenceRealizationTarget {
    External(ExternalCallableId),
    Invalid,
}

/// Materialize the physical target for an exact provider-selected member intrinsic that has no JVM
/// method to reference. The generated static helper is an implementation detail of this backend;
/// the surrounding `FuncRef` continues to describe the original Kotlin declaration for reflection
/// and equality.
fn intrinsic_member_adapter(
    ir: &mut IrFile,
    realization: MemberRealization,
    receiver: Ty,
    parameters: &[Ty],
    result: Ty,
) -> Option<(crate::ir::FunId, String)> {
    let receiver_value = ir.add_expr(IrExpr::GetValue(0));
    let arguments = parameters
        .iter()
        .enumerate()
        .map(|(parameter, _)| ir.add_expr(IrExpr::GetValue(parameter as u32 + 1)))
        .collect::<Vec<_>>();
    let value =
        match realization {
            MemberRealization::Intrinsic(crate::libraries::CompilerIntrinsic::BooleanNot)
                if parameters.is_empty()
                    && receiver.non_null() == Ty::Boolean
                    && result == Ty::Boolean =>
            {
                let false_value = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(false)));
                ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Eq,
                    lhs: receiver_value,
                    rhs: false_value,
                })
            }
            MemberRealization::Intrinsic(
                crate::libraries::CompilerIntrinsic::NumericConversion,
            ) if parameters.is_empty() => ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg: receiver_value,
                type_operand: result,
            }),
            MemberRealization::Intrinsic(crate::libraries::CompilerIntrinsic::StringPlus)
                if arguments.len() == 1 =>
            {
                ir.add_expr(IrExpr::Call {
                    callee: crate::ir::Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::StringPlus,
                        ret: result,
                    },
                    dispatch_receiver: Some(receiver_value),
                    args: arguments,
                })
            }
            _ => return None,
        };
    let returned = ir.add_expr(IrExpr::Return(Some(value)));
    let body = ir.add_expr(IrExpr::Block {
        stmts: vec![returned],
        value: None,
    });
    let name = format!("$fir$intrinsic$fnref${}", ir.functions.len());
    let function = ir.add_fun(IrFunction {
        name: name.clone(),
        params: std::iter::once(receiver)
            .chain(parameters.iter().copied())
            .collect(),
        ret: result,
        body: Some(body),
        is_static: true,
        dispatch_receiver: None,
        param_checks: Vec::new(),
    });
    ir.private_methods.insert(function);
    Some((function, name))
}

pub(super) fn realize(
    ir: &mut IrFile,
    classpath: &Classpath,
    current_facade: &str,
) -> Result<(), FunctionReferenceRealizationTarget> {
    let expression_count = ir.exprs.len();
    for raw in 0..expression_count {
        let IrExpr::Checked(IrCheckedOperation::CallableReference {
            target,
            binding,
            dispatch_receiver,
            extension_receiver,
            function_type,
            substitutions: _,
            adaptation,
        }) = ir.exprs[raw].clone()
        else {
            continue;
        };
        if adaptation.is_some() || dispatch_receiver.is_some() && extension_receiver.is_some() {
            return Err(FunctionReferenceRealizationTarget::Invalid);
        }
        let FirCallableReferenceTarget::External {
            declaration,
            receiver,
            extension_receiver: target_is_extension,
            parameters,
            result,
            ..
        } = target
        else {
            return Err(FunctionReferenceRealizationTarget::Invalid);
        };
        let realization = classpath
            .external_callable(declaration)
            .ok_or(FunctionReferenceRealizationTarget::External(declaration))?;
        let callable = realization.callable;
        let Ty::Fun(reference) = function_type.non_null() else {
            return Err(FunctionReferenceRealizationTarget::Invalid);
        };
        if reference.ret != result.get() || callable.suspend != reference.suspend {
            return Err(FunctionReferenceRealizationTarget::External(declaration));
        }

        let capture = dispatch_receiver.or(extension_receiver);
        let receiver_ty = receiver.map(crate::fir::ResolvedTy::get);
        let semantic_parameters = parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        let mut local_target = None;
        let mut call_owner = Some(callable.owner);
        let mut call_name = callable.name.clone();
        let mut call_interface = callable.owner_is_interface;
        let (bound, dispatch, owner_class, flags, mut target_parameters, reference_receiver) =
            match (realization.kind, target_is_extension, binding) {
                (ExternalCallableKind::TopLevel, false, FirCallableReferenceBinding::Static) => {
                    if receiver_ty.is_some() || capture.is_some() {
                        return Err(FunctionReferenceRealizationTarget::Invalid);
                    }
                    (
                        false,
                        FrDispatch::Static,
                        Some(callable.owner),
                        1,
                        callable.physical_params.clone(),
                        false,
                    )
                }
                (ExternalCallableKind::Member, false, FirCallableReferenceBinding::Bound) => {
                    if dispatch_receiver.is_none() || extension_receiver.is_some() {
                        return Err(FunctionReferenceRealizationTarget::Invalid);
                    }
                    if callable.member_realization == MemberRealization::Dispatch {
                        (
                            true,
                            FrDispatch::VirtualBound,
                            receiver_ty.and_then(Ty::kotlin_class_internal),
                            0,
                            callable.physical_params.clone(),
                            false,
                        )
                    } else {
                        let receiver =
                            receiver_ty.ok_or(FunctionReferenceRealizationTarget::Invalid)?;
                        let (target, name) = intrinsic_member_adapter(
                            ir,
                            callable.member_realization,
                            receiver,
                            &semantic_parameters,
                            result.get(),
                        )
                        .ok_or(FunctionReferenceRealizationTarget::Invalid)?;
                        local_target = Some(target);
                        call_owner = None;
                        call_name = name;
                        call_interface = false;
                        (
                            true,
                            FrDispatch::StaticBound,
                            receiver.kotlin_class_internal(),
                            0,
                            std::iter::once(receiver)
                                .chain(semantic_parameters.iter().copied())
                                .collect(),
                            false,
                        )
                    }
                }
                (ExternalCallableKind::Member, false, FirCallableReferenceBinding::Unbound) => {
                    let receiver_ty =
                        receiver_ty.ok_or(FunctionReferenceRealizationTarget::Invalid)?;
                    if capture.is_some() || reference.params.first().copied() != Some(receiver_ty) {
                        return Err(FunctionReferenceRealizationTarget::Invalid);
                    }
                    if callable.member_realization == MemberRealization::Dispatch {
                        let mut target = Vec::with_capacity(callable.physical_params.len() + 1);
                        target.push(receiver_ty);
                        target.extend(callable.physical_params.iter().copied());
                        (
                            false,
                            FrDispatch::VirtualUnbound,
                            receiver_ty.kotlin_class_internal(),
                            0,
                            target,
                            true,
                        )
                    } else {
                        let (target, name) = intrinsic_member_adapter(
                            ir,
                            callable.member_realization,
                            receiver_ty,
                            &semantic_parameters,
                            result.get(),
                        )
                        .ok_or(FunctionReferenceRealizationTarget::Invalid)?;
                        local_target = Some(target);
                        call_owner = None;
                        call_name = name;
                        call_interface = false;
                        (
                            false,
                            FrDispatch::Static,
                            receiver_ty.kotlin_class_internal(),
                            0,
                            std::iter::once(receiver_ty)
                                .chain(semantic_parameters.iter().copied())
                                .collect(),
                            true,
                        )
                    }
                }
                (ExternalCallableKind::Extension, true, FirCallableReferenceBinding::Bound) => {
                    if extension_receiver.is_none() || dispatch_receiver.is_some() {
                        return Err(FunctionReferenceRealizationTarget::Invalid);
                    }
                    (
                        true,
                        FrDispatch::StaticBound,
                        receiver_ty.and_then(Ty::kotlin_class_internal),
                        1,
                        callable.physical_params.clone(),
                        false,
                    )
                }
                (ExternalCallableKind::Extension, true, FirCallableReferenceBinding::Unbound) => {
                    let receiver_ty =
                        receiver_ty.ok_or(FunctionReferenceRealizationTarget::Invalid)?;
                    if capture.is_some() || reference.params.first().copied() != Some(receiver_ty) {
                        return Err(FunctionReferenceRealizationTarget::Invalid);
                    }
                    (
                        false,
                        FrDispatch::Static,
                        receiver_ty.kotlin_class_internal(),
                        1,
                        callable.physical_params.clone(),
                        true,
                    )
                }
                _ => return Err(FunctionReferenceRealizationTarget::Invalid),
            };

        if parameters.len() + usize::from(reference_receiver) != reference.params.len()
            || matches!(dispatch, FrDispatch::StaticBound) && target_parameters.is_empty()
        {
            return Err(FunctionReferenceRealizationTarget::External(declaration));
        }
        let mut invoke_parameters = reference.params.clone();
        let mut invoke_result = reference.ret;
        let mut target_result = callable.physical_ret;
        if reference.suspend {
            let continuation = Ty::obj("kotlin/coroutines/Continuation");
            invoke_parameters.push(continuation);
            target_parameters.push(continuation);
            invoke_result = Ty::obj("kotlin/Any");
            target_result = Ty::obj("kotlin/Any");
        }
        let arity = u8::try_from(reference.params.len())
            .map_err(|_| FunctionReferenceRealizationTarget::External(declaration))?;
        let internal = type_name(&format!(
            "{current_facade}$fir$function${}",
            ir.classes.len()
        ));
        let mut class = IrClass::synthetic(internal);
        class.superclass = type_name("kotlin/jvm/internal/FunctionReferenceImpl");
        class.func_ref = Some(FuncRef {
            adapted: false,
            bound,
            arity,
            is_suspend: reference.suspend,
            module_target: None,
            local_target,
            owner_class,
            fn_name: callable
                .reflection_name
                .clone()
                .unwrap_or_else(|| callable.name.clone()),
            flags,
            dispatch,
            call_owner,
            call_name,
            reflection_name: None,
            reflection_receiver_parameter: false,
            // The selected provider realization above supplies physical call parameters, while
            // callable-reference reflection identifies the Kotlin declaration. Preserve its
            // semantic signature separately so the value-class pass mangles the reflected JVM
            // signature from `Marker`, not from its already-erased `String` carrier.
            reflection_target_ret_ty: Some(result.get()),
            reflection_target_param_tys: Some(semantic_parameters),
            call_interface,
            param_tys: invoke_parameters,
            ret_ty: invoke_result,
            target_param_tys: target_parameters,
            target_ret_ty: target_result,
            unbox_params: vec![None; arity as usize],
            unbox_param_nullable: vec![false; arity as usize],
            box_ret: None,
            staticbound_recv_unbox: None,
        });
        let class = ir.add_class(class);
        ir.exprs[raw] = match capture {
            Some(capture) => IrExpr::New {
                internal,
                args: vec![capture],
                ctor_params: Some(vec![Ty::obj("kotlin/Any")]),
                ctor_desc: None,
                external_target: None,
                defaults: Box::new([]),
                default_prefix_count: 0,
            },
            None => IrExpr::StaticInstance {
                owner: class,
                ty: class,
                field: "INSTANCE",
            },
        };
    }
    Ok(())
}
