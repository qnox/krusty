//! Structural realization of checked same-module callable references.
//!
//! A callable reference is not an ordinary lambda: Kotlin's runtime equality compares its selected
//! declaration, signature, and bound receiver. This module converts the stable FIR selection into the
//! existing backend-neutral `FuncRef` plan; it performs no lookup or overload selection.

use crate::fir::{
    FirCallableReferenceBinding, ResolvedCallableHeader, ResolvedClassifierHeader,
    ResolvedSignature,
};
use crate::ir::{FrDispatch, FuncRef, IrClass, IrExpr, IrFunction};
use crate::types::{type_name, FnSig, Ty};

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    pub(super) fn checked_extension_function_binding(
        &mut self,
        receiver: crate::fir::FirReceiver,
        callable: crate::fir::FirExprId,
        target_parameters: &[crate::fir::ResolvedTy],
        receiver_parameter: u32,
        target_result: crate::fir::ResolvedTy,
        suspend: bool,
    ) -> Result<crate::ir::ExprId, FirLoweringFailure> {
        let fir_callable = callable;
        let receiver_parameter = usize::try_from(receiver_parameter)
            .map_err(|_| FirLoweringFailure::MissingExpression(callable))?;
        let receiver_type = target_parameters
            .get(receiver_parameter)
            .copied()
            .ok_or(FirLoweringFailure::MissingExpression(fir_callable))?;
        let callable_type = self
            .body
            .expr(callable)
            .ok_or(FirLoweringFailure::MissingExpression(callable))?
            .ty
            .get();

        // Captures retain source evaluation order: `receiver` is evaluated before the parenthesized
        // callable expression. The wrapper's first two value slots mirror that order.
        let receiver = self.expression_with_conversion(receiver.value, receiver.conversion)?;
        let callable = self.expression(fir_callable)?;
        let function_value = self.ir.add_expr(IrExpr::GetValue(1));
        let bound_receiver = self.ir.add_expr(IrExpr::GetValue(0));
        let mut next_parameter = 2u32;
        let arguments = target_parameters
            .iter()
            .enumerate()
            .map(|(parameter, _)| {
                if parameter == receiver_parameter {
                    bound_receiver
                } else {
                    let value = self.ir.add_expr(IrExpr::GetValue(next_parameter));
                    next_parameter = next_parameter
                        .checked_add(1)
                        .expect("too many bound extension-function parameters");
                    value
                }
            })
            .collect::<Vec<_>>();
        let invoke = self.ir.add_expr(IrExpr::InvokeFunction {
            func: function_value,
            args: arguments,
            params: target_parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect(),
            ret: target_result.get(),
        });
        if suspend {
            self.ir.suspend_calls.insert(invoke, target_result.get());
        }
        let body =
            self.callable_reference_adapter_body(invoke, target_result.get(), target_result.get());
        let bound_parameters = target_parameters
            .iter()
            .enumerate()
            .filter_map(|(parameter, ty)| (parameter != receiver_parameter).then_some(ty.get()))
            .collect::<Vec<_>>();
        let mut parameters = Vec::with_capacity(bound_parameters.len() + 2);
        parameters.push(receiver_type.get());
        parameters.push(callable_type);
        parameters.extend(bound_parameters.iter().copied());
        let wrapper = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_extension_bind_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: parameters,
            ret: crate::types::stored_value_ty(target_result.get()),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        self.ir.private_methods.insert(wrapper);
        self.ir.lambda_own_params_from.insert(wrapper, 2);
        if suspend {
            self.ir.suspend_funs.push(wrapper);
        }
        if let Some(owner) = self
            .index
            .enclosing_classifier(crate::fir::DeclarationId::from_raw(self.body.owner().raw()))
        {
            if let Some(class) = self
                .ir
                .checked_classifier_classes
                .get(&owner.declaration)
                .copied()
            {
                self.ir.classes[class as usize].methods.push(wrapper);
            }
        }
        let arity = bound_parameters
            .len()
            .checked_add(usize::from(suspend))
            .and_then(|arity| u8::try_from(arity).ok())
            .ok_or(FirLoweringFailure::MissingExpression(fir_callable))?;
        Ok(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: wrapper,
            arity,
            captures: vec![receiver, callable],
            sam: None,
            inline_body: None,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn checked_function_invoke_reference(
        &mut self,
        callee: crate::fir::FirExprId,
        target_parameters: &[crate::fir::ResolvedTy],
        target_result: crate::fir::ResolvedTy,
        target_suspend: bool,
        reference_parameters: &[crate::fir::ResolvedTy],
        reference_result: crate::fir::ResolvedTy,
        suspend: bool,
    ) -> Result<crate::ir::ExprId, FirLoweringFailure> {
        if target_parameters.len() != reference_parameters.len() || (target_suspend && !suspend) {
            return Err(FirLoweringFailure::MissingExpression(callee));
        }
        let callee_type = self
            .body
            .expr(callee)
            .ok_or(FirLoweringFailure::MissingExpression(callee))?
            .ty
            .get();
        let captured = self.expression(callee)?;
        let function_value = self.ir.add_expr(IrExpr::GetValue(0));
        let arguments = reference_parameters
            .iter()
            .enumerate()
            .map(|(parameter, _)| {
                self.ir.add_expr(IrExpr::GetValue(
                    u32::try_from(parameter + 1).expect("too many invoke-reference parameters"),
                ))
            })
            .collect::<Vec<_>>();
        let invoke = self.ir.add_expr(IrExpr::InvokeFunction {
            func: function_value,
            args: arguments,
            params: target_parameters
                .iter()
                .map(|parameter| parameter.get())
                .collect(),
            ret: target_result.get(),
        });
        if target_suspend {
            self.ir.suspend_calls.insert(invoke, target_result.get());
        }
        let body = self.callable_reference_adapter_body(
            invoke,
            target_result.get(),
            reference_result.get(),
        );
        let mut parameters = Vec::with_capacity(reference_parameters.len() + 1);
        parameters.push(callee_type);
        parameters.extend(reference_parameters.iter().map(|parameter| parameter.get()));
        let wrapper = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_invoke_ref_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            params: parameters,
            ret: crate::types::stored_value_ty(reference_result.get()),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        self.ir.private_methods.insert(wrapper);
        self.ir.lambda_own_params_from.insert(wrapper, 1);
        if suspend {
            self.ir.suspend_funs.push(wrapper);
        }
        if let Some(owner) = self
            .index
            .enclosing_classifier(crate::fir::DeclarationId::from_raw(self.body.owner().raw()))
        {
            if let Some(class) = self
                .ir
                .checked_classifier_classes
                .get(&owner.declaration)
                .copied()
            {
                self.ir.classes[class as usize].methods.push(wrapper);
            }
        }
        let arity = reference_parameters
            .len()
            .checked_add(usize::from(suspend))
            .and_then(|arity| u8::try_from(arity).ok())
            .ok_or(FirLoweringFailure::MissingExpression(callee))?;
        Ok(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: wrapper,
            arity,
            captures: vec![captured],
            sam: None,
            inline_body: None,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn materialize_structural_module_function_reference(
        &mut self,
        callable: ResolvedCallableHeader,
        binding: FirCallableReferenceBinding,
        dispatch_capture: Option<crate::ir::ExprId>,
        extension_capture: Option<crate::ir::ExprId>,
        reference: &FnSig,
        signature: &ResolvedSignature,
        enclosing: Option<&ResolvedClassifierHeader>,
    ) -> Result<Option<crate::ir::ExprId>, FirLoweringFailure> {
        let companion_extension = self
            .index
            .declaration_header(callable.declaration)
            .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::COMPANION));
        let extension_receiver = (!companion_extension)
            .then_some(callable.shape.extension_receiver)
            .flatten();
        // A member extension can bind two independent receivers. The existing structural carrier has
        // one bound-receiver slot, so retain the checked wrapper realization until its carrier grows a
        // tuple capture; do not approximate the identity or invocation shape.
        if enclosing.is_some() && extension_receiver.is_some() {
            return Ok(None);
        }

        let function = self
            .ir
            .checked_callable_functions
            .get(&callable.id)
            .copied();
        if function.is_some_and(|function| self.ir.foreign_inline_templates.contains(&function)) {
            return Ok(None);
        }
        let reflection_name = self
            .index
            .callable_name(callable.id)
            .ok_or(FirLoweringFailure::MissingCallable(callable.id))?
            .to_owned();
        // A sibling member can be invoked directly from its stable enclosing-class identity. A
        // sibling top-level callable additionally needs its file-facade identity, which is realized
        // later by the backend and therefore continues through the checked wrapper path.
        if function.is_none() && enclosing.is_none() {
            return Ok(None);
        }
        let mut call_name = function
            .map(|function| self.ir.functions[function as usize].name.clone())
            .unwrap_or_else(|| reflection_name.clone());

        let (bound, capture, mut dispatch, owner_class, call_owner, flags, mut target_parameters) =
            match (enclosing, extension_receiver, binding) {
                (None, None, FirCallableReferenceBinding::Static) => (
                    false,
                    None,
                    FrDispatch::Static,
                    None,
                    None,
                    1,
                    signature
                        .parameters
                        .iter()
                        .map(|parameter| parameter.get())
                        .collect::<Vec<_>>(),
                ),
                (None, Some(receiver), FirCallableReferenceBinding::Bound) => {
                    let Some(capture) = extension_capture else {
                        return Ok(None);
                    };
                    let mut parameters = vec![receiver.get()];
                    parameters.extend(signature.parameters.iter().map(|parameter| parameter.get()));
                    (
                        true,
                        Some(capture),
                        FrDispatch::StaticBound,
                        None,
                        None,
                        1,
                        parameters,
                    )
                }
                (None, Some(receiver), FirCallableReferenceBinding::Unbound) => {
                    let mut parameters = vec![receiver.get()];
                    parameters.extend(signature.parameters.iter().map(|parameter| parameter.get()));
                    (false, None, FrDispatch::Static, None, None, 1, parameters)
                }
                (Some(owner), None, FirCallableReferenceBinding::Bound) => {
                    let Some(capture) = dispatch_capture else {
                        return Ok(None);
                    };
                    (
                        true,
                        Some(capture),
                        FrDispatch::VirtualBound,
                        Some(owner.classifier),
                        Some(owner.classifier),
                        0,
                        signature
                            .parameters
                            .iter()
                            .map(|parameter| parameter.get())
                            .collect(),
                    )
                }
                (Some(owner), None, FirCallableReferenceBinding::Unbound) => {
                    let mut parameters = vec![Ty::obj_name(owner.classifier)];
                    parameters.extend(signature.parameters.iter().map(|parameter| parameter.get()));
                    (
                        false,
                        None,
                        FrDispatch::VirtualUnbound,
                        Some(owner.classifier),
                        Some(owner.classifier),
                        0,
                        parameters,
                    )
                }
                _ => return Ok(None),
            };

        let mut physical_reflection_name = None;
        let mut reflection_receiver_parameter = false;
        if let (Some(function), Some(owner)) = (
            function,
            enclosing.filter(|_| {
                function.is_some_and(|function| self.ir.private_methods.contains(&function))
            }),
        ) {
            self.ir.function_reference_access_bridges.insert(function);
            physical_reflection_name = Some(call_name.clone());
            call_name = format!("access${call_name}");
            reflection_receiver_parameter = true;
            match dispatch {
                FrDispatch::VirtualBound => {
                    dispatch = FrDispatch::StaticBound;
                    target_parameters.insert(0, Ty::obj_name(owner.classifier));
                }
                FrDispatch::VirtualUnbound => dispatch = FrDispatch::Static,
                FrDispatch::Static | FrDispatch::StaticBound | FrDispatch::SuspendConvert => {}
            }
        }

        self.finish_structural_module_function_reference(
            callable,
            reference,
            signature,
            bound,
            capture,
            dispatch,
            owner_class,
            call_owner,
            flags,
            target_parameters,
            call_name,
            reflection_name,
            physical_reflection_name,
            reflection_receiver_parameter,
            enclosing,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_structural_module_function_reference(
        &mut self,
        callable: ResolvedCallableHeader,
        reference: &FnSig,
        signature: &ResolvedSignature,
        bound: bool,
        capture: Option<crate::ir::ExprId>,
        dispatch: FrDispatch,
        owner_class: Option<crate::types::TypeName>,
        call_owner: Option<crate::types::TypeName>,
        flags: i32,
        mut target_parameters: Vec<Ty>,
        call_name: String,
        reflection_name: String,
        physical_reflection_name: Option<String>,
        reflection_receiver_parameter: bool,
        enclosing: Option<&ResolvedClassifierHeader>,
    ) -> Result<Option<crate::ir::ExprId>, FirLoweringFailure> {
        if capture.is_some()
            != matches!(dispatch, FrDispatch::VirtualBound | FrDispatch::StaticBound)
            || target_parameters.len() < reference.params.len()
        {
            return Ok(None);
        }
        let suspend = self
            .index
            .declaration_header(callable.declaration)
            .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::SUSPEND));
        if suspend != reference.suspend {
            return Ok(None);
        }
        let mut invoke_parameters = reference.params.clone();
        let mut invoke_result = reference.ret;
        let mut target_result = signature.result.get();
        if suspend {
            let continuation = Ty::obj("kotlin/coroutines/Continuation");
            invoke_parameters.push(continuation);
            target_parameters.push(continuation);
            invoke_result = Ty::obj("kotlin/Any");
            target_result = Ty::obj("kotlin/Any");
        }

        let simple_name = format!(
            "$fir$fnref${}_{}",
            self.body.owner().raw(),
            self.ir.classes.len()
        );
        let internal = self.ir.package.as_ref().map_or_else(
            || type_name(&simple_name),
            |package| type_name(&format!("{}/{}", package.replace('.', "/"), simple_name)),
        );
        let mut class = IrClass::synthetic(internal);
        class.superclass = type_name("kotlin/jvm/internal/FunctionReferenceImpl");
        class.func_ref = Some(FuncRef {
            adapted: false,
            bound,
            arity: u8::try_from(invoke_parameters.len())
                .map_err(|_| FirLoweringFailure::UnsupportedCallableReference(callable.id))?,
            is_suspend: suspend,
            module_target: Some(callable.id),
            local_target: None,
            owner_class,
            fn_name: reflection_name,
            flags,
            dispatch,
            call_owner,
            call_name,
            reflection_name: physical_reflection_name,
            reflection_receiver_parameter,
            reflection_target_ret_ty: None,
            reflection_target_param_tys: None,
            call_interface: enclosing.is_some_and(|owner| {
                self.index
                    .declaration_header(owner.declaration)
                    .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::INTERFACE))
            }),
            param_tys: invoke_parameters.clone(),
            ret_ty: invoke_result,
            target_param_tys: target_parameters,
            target_ret_ty: target_result,
            unbox_params: vec![None; invoke_parameters.len()],
            unbox_param_nullable: vec![false; invoke_parameters.len()],
            box_ret: None,
            staticbound_recv_unbox: None,
        });
        let class = self.ir.add_class(class);
        Ok(Some(match capture {
            Some(capture) => self.ir.add_expr(IrExpr::New {
                internal,
                args: vec![capture],
                ctor_params: Some(vec![Ty::obj("kotlin/Any")]),
                ctor_desc: None,
                external_target: None,
            }),
            None => self.ir.add_expr(IrExpr::StaticInstance {
                owner: class,
                ty: class,
                field: "INSTANCE",
            }),
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn materialize_structural_adapted_module_reference(
        &mut self,
        callable: ResolvedCallableHeader,
        reference: &FnSig,
        original_parameters: &[Ty],
        original_result: Ty,
        wrapper_name: String,
        wrapper: crate::ir::FunId,
        mut wrapper_parameters: Vec<Ty>,
        capture: Option<crate::ir::ExprId>,
        adaptation: &crate::fir::FirReferenceAdaptation,
        enclosing: Option<&ResolvedClassifierHeader>,
    ) -> Result<crate::ir::ExprId, FirLoweringFailure> {
        let mut invoke_parameters = reference.params.clone();
        let mut invoke_result = reference.ret;
        let mut target_result = reference.ret;
        if reference.suspend {
            let continuation = Ty::obj("kotlin/coroutines/Continuation");
            invoke_parameters.push(continuation);
            wrapper_parameters.push(continuation);
            invoke_result = Ty::obj("kotlin/Any");
            target_result = Ty::obj("kotlin/Any");
        }
        let mut reflection_parameters = original_parameters.to_vec();
        let mut reflection_result = original_result;
        let declaration_suspend = self
            .index
            .declaration_header(callable.declaration)
            .is_some_and(|header| header.flags.has(crate::fir::DeclarationFlags::SUSPEND));
        if declaration_suspend {
            reflection_parameters.push(Ty::obj("kotlin/coroutines/Continuation"));
            reflection_result = Ty::obj("kotlin/Any");
        }

        let vararg_conversion = adaptation.arguments.iter().any(|argument| {
            matches!(
                argument,
                crate::fir::FirAdaptedReferenceArgument::Vararg {
                    whole_array: false,
                    ..
                }
            )
        });
        let unit_conversion =
            adaptation.result_type.get() == Ty::Unit && original_result != Ty::Unit;
        let adaptation_flags = i32::from(vararg_conversion)
            | (i32::from(adaptation.suspend_conversion) << 1)
            | (i32::from(unit_conversion) << 2);
        let top_level = i32::from(enclosing.is_none());
        let flags = top_level | (adaptation_flags << 1);
        let bound = capture.is_some();
        let dispatch = if bound {
            FrDispatch::StaticBound
        } else {
            FrDispatch::Static
        };
        if wrapper_parameters.len() != invoke_parameters.len() + usize::from(bound) {
            return Err(FirLoweringFailure::UnsupportedCallableReference(
                callable.id,
            ));
        }

        let name = self
            .index
            .callable_name(callable.id)
            .ok_or(FirLoweringFailure::MissingCallable(callable.id))?
            .to_owned();
        let simple_name = format!(
            "$fir$adapted$fnref${}_{}",
            self.body.owner().raw(),
            self.ir.classes.len()
        );
        let internal = self.ir.package.as_ref().map_or_else(
            || type_name(&simple_name),
            |package| type_name(&format!("{}/{}", package.replace('.', "/"), simple_name)),
        );
        let mut class = IrClass::synthetic(internal);
        class.superclass = type_name("kotlin/jvm/internal/AdaptedFunctionReference");
        class.func_ref = Some(FuncRef {
            adapted: true,
            bound,
            arity: u8::try_from(invoke_parameters.len())
                .map_err(|_| FirLoweringFailure::UnsupportedCallableReference(callable.id))?,
            is_suspend: reference.suspend,
            module_target: None,
            local_target: Some(wrapper),
            owner_class: enclosing.map(|owner| owner.classifier),
            fn_name: name,
            flags,
            dispatch,
            call_owner: None,
            call_name: wrapper_name,
            reflection_name: None,
            reflection_receiver_parameter: false,
            reflection_target_ret_ty: Some(reflection_result),
            reflection_target_param_tys: Some(reflection_parameters),
            call_interface: false,
            param_tys: invoke_parameters.clone(),
            ret_ty: invoke_result,
            target_param_tys: wrapper_parameters,
            target_ret_ty: target_result,
            unbox_params: vec![None; invoke_parameters.len()],
            unbox_param_nullable: vec![false; invoke_parameters.len()],
            box_ret: None,
            staticbound_recv_unbox: None,
        });
        let class = self.ir.add_class(class);
        Ok(match capture {
            Some(capture) => self.ir.add_expr(IrExpr::New {
                internal,
                args: vec![capture],
                ctor_params: Some(vec![Ty::obj("kotlin/Any")]),
                ctor_desc: None,
                external_target: None,
            }),
            None => self.ir.add_expr(IrExpr::StaticInstance {
                owner: class,
                ty: class,
                field: "INSTANCE",
            }),
        })
    }
}
