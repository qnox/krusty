use std::collections::HashMap;

use crate::fir::{
    FirBody, FirCapture, FirImplicitReceiverCapture, FirLocalCallableRef, FirStatementId,
    FirStatementKind, LocalCallableId,
};
use crate::ir::{Callee, ExprId, FrDispatch, FuncRef, IrClass, IrExpr, IrFunction};
use crate::types::{type_name, Ty};

use super::checked_arguments::{
    materialize_checked_arguments, CheckedArgumentSlot, CheckedArgumentValue,
};
use super::{
    finish_callable_body, BodyLowering, CaptureSlot, FirLoweringFailure, LocalCallableRealization,
};

impl BodyLowering<'_> {
    pub(super) fn prepare_local_functions(&mut self) -> Result<(), FirLoweringFailure> {
        for raw in 0..self.body.statement_count() {
            let statement_id = FirStatementId::from_raw(
                u32::try_from(raw).expect("too many FIR statements for a stable id"),
            );
            let statement = self
                .body
                .statement(statement_id)
                .ok_or(FirLoweringFailure::MissingStatement(statement_id))?;
            let FirStatementKind::LocalFunction {
                declaration,
                callable,
                suspend,
                body,
            } = &statement.kind
            else {
                continue;
            };
            let (function, owner) = self.predeclare_local_function(body, *callable)?;
            if *suspend && !self.ir.suspend_funs.contains(&function) {
                self.ir.suspend_funs.push(function);
            }
            let realization = LocalCallableRealization {
                function,
                suspend: *suspend,
                owner,
                source_name: body.debug_name().unwrap_or("<anonymous>").into(),
                captures: body.captures().to_vec(),
                implicit_receiver_captures: body.implicit_receiver_captures().to_vec(),
                context_parameter_count: body.context_receiver_types().len() as u32,
                has_extension_receiver: body.receiver_type().is_some(),
            };
            let previous = self
                .local_callable_scopes
                .last_mut()
                .expect("a body always has a local callable scope")
                .insert(*callable, realization.clone());
            assert!(previous.is_none(), "a FIR local callable is declared once");
            let previous = self
                .published_local_callables
                .insert(*declaration, realization);
            assert!(
                previous.is_none(),
                "a body-local declaration has one common-IR realization"
            );
        }
        Ok(())
    }

    pub(super) fn realize_local_functions(&mut self) -> Result<(), FirLoweringFailure> {
        let declarations = (0..self.body.statement_count())
            .filter_map(|raw| {
                let statement = FirStatementId::from_raw(u32::try_from(raw).ok()?);
                let FirStatementKind::LocalFunction { callable, .. } =
                    &self.body.statement(statement)?.kind
                else {
                    return None;
                };
                Some((statement, *callable))
            })
            .collect::<Vec<_>>();
        for (statement, callable) in declarations {
            let function = self
                .local_callable_scopes
                .last()
                .and_then(|scope| scope.get(&callable))
                .map(|realization| realization.function)
                .ok_or(FirLoweringFailure::MissingLocalCallable(
                    FirLocalCallableRef {
                        body_depth: 0,
                        callable,
                        declaration: None,
                        external_capture_arguments: None,
                    },
                ))?;
            let FirStatementKind::LocalFunction { body, .. } = &self
                .body
                .statement(statement)
                .ok_or(FirLoweringFailure::MissingStatement(statement))?
                .kind
            else {
                unreachable!("local function declaration changed during FIR lowering")
            };
            let lowered = self.lower_nested_function(body, function, false, false)?;
            self.ir.functions[function as usize].body = Some(lowered.callable);
        }
        Ok(())
    }

    pub(super) fn captured_value(
        &mut self,
        enclosing_depth: u32,
        source: crate::fir::LocalValueId,
    ) -> Result<ExprId, FirLoweringFailure> {
        let capture = self
            .capture_slots
            .get(&(enclosing_depth, source))
            .copied()
            .ok_or(FirLoweringFailure::MissingCapture {
                enclosing_depth,
                source,
            })?;
        let holder = self.ir.add_expr(IrExpr::GetValue(capture.slot));
        if capture.shared_cell {
            Ok(self.ir.add_expr(IrExpr::RefGet {
                elem: capture.ty.get(),
                holder,
            }))
        } else {
            Ok(holder)
        }
    }

    pub(super) fn checked_local_call(
        &mut self,
        target: FirLocalCallableRef,
        extension_receiver: Option<crate::fir::FirReceiver>,
        arguments: &[crate::fir::FirCallArgument],
    ) -> Result<ExprId, FirLoweringFailure> {
        let (realization, external) = self.local_function(&target)?;
        let captures = if external {
            target
                .external_capture_arguments
                .as_deref()
                .ok_or_else(|| FirLoweringFailure::MissingLocalCallable(target.clone()))?
                .iter()
                .copied()
                .map(|argument| self.expression(argument))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            self.bound_all_captures(target.body_depth, &realization)?
        };
        let extension_receiver = extension_receiver
            .map(|receiver| self.expression_with_conversion(receiver.value, receiver.conversion))
            .transpose()?;
        let mut logical_parameter_types = self.ir.functions[realization.function as usize].params
            [realization.capture_count()..]
            .to_vec();
        if realization.has_extension_receiver {
            logical_parameter_types.remove(realization.context_parameter_count as usize);
        }
        let arguments = self.lower_arguments(arguments, |parameter| {
            logical_parameter_types
                .get(parameter as usize)
                .copied()
                .ok_or(FirLoweringFailure::MissingExternalParameter { parameter })
        })?;
        self.materialize_local_call(
            &target,
            &realization,
            captures,
            extension_receiver,
            &arguments,
        )
    }

    fn materialize_local_call(
        &mut self,
        target: &FirLocalCallableRef,
        realization: &LocalCallableRealization,
        captures: Vec<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[crate::ir::IrCheckedArgument],
    ) -> Result<ExprId, FirLoweringFailure> {
        let logical_count = self.ir.functions[realization.function as usize]
            .params
            .len()
            .checked_sub(realization.capture_count())
            .ok_or_else(|| FirLoweringFailure::MissingLocalCallable(target.clone()))?;
        let mut slots = materialize_checked_arguments(
            arguments,
            logical_count,
            |parameter| {
                let parameter = parameter as usize;
                Some(
                    parameter
                        + usize::from(
                            realization.has_extension_receiver
                                && parameter >= realization.context_parameter_count as usize,
                        ),
                )
            },
            |_, argument| match argument {
                CheckedArgumentValue::Expression(value)
                | CheckedArgumentValue::VarargElement { value, .. } => Some(value),
            },
        )
        .ok_or_else(|| FirLoweringFailure::MissingLocalCallable(target.clone()))?;
        if realization.has_extension_receiver {
            let position = realization.context_parameter_count as usize;
            let slot = slots
                .get_mut(position)
                .ok_or_else(|| FirLoweringFailure::MissingLocalCallable(target.clone()))?;
            if !matches!(slot, CheckedArgumentSlot::Missing) {
                return Err(FirLoweringFailure::MissingLocalCallable(target.clone()));
            }
            *slot = CheckedArgumentSlot::Expression(
                extension_receiver
                    .ok_or_else(|| FirLoweringFailure::MissingLocalCallable(target.clone()))?,
            );
        } else if extension_receiver.is_some() {
            return Err(FirLoweringFailure::MissingLocalCallable(target.clone()));
        }
        let slots = slots
            .into_iter()
            .map(|slot| match slot {
                CheckedArgumentSlot::Expression(value) => Some(value),
                CheckedArgumentSlot::Vararg {
                    array_type,
                    elements,
                    spreads,
                } => Some(self.ir.add_expr(IrExpr::Vararg {
                    array_type,
                    elements,
                    spreads,
                })),
                CheckedArgumentSlot::Default(_) | CheckedArgumentSlot::Missing => None,
            })
            .collect::<Vec<_>>();
        let omitted = slots.iter().any(Option::is_none);
        let mut physical = captures.into_iter().map(Some).collect::<Vec<_>>();
        physical.extend(slots);
        let (callee, args) = if omitted {
            let defaults = self.ir.param_defaults(realization.function).ok_or(
                FirLoweringFailure::MissingLocalDefault {
                    function: realization.function,
                    parameter: 0,
                },
            )?;
            for (parameter, slot) in physical.iter().enumerate() {
                if slot.is_none()
                    && defaults
                        .get(parameter)
                        .is_none_or(|default| default.is_none())
                {
                    return Err(FirLoweringFailure::MissingLocalDefault {
                        function: realization.function,
                        parameter: u32::try_from(parameter)
                            .expect("too many lifted local parameters"),
                    });
                }
            }
            let default_arguments = physical
                .iter()
                .enumerate()
                .filter_map(|(parameter, argument)| {
                    argument
                        .is_none()
                        .then(|| u32::try_from(parameter).ok())
                        .flatten()
                })
                .collect::<Vec<_>>()
                .into_boxed_slice();
            let args = physical.into_iter().flatten().collect();
            (
                realization.owner.map_or(
                    Callee::LocalWithDefaults {
                        function: realization.function,
                        defaults: default_arguments.clone(),
                    },
                    |owner| Callee::ClassStaticWithDefaults {
                        owner,
                        function: realization.function,
                        defaults: default_arguments,
                    },
                ),
                args,
            )
        } else {
            (
                realization
                    .owner
                    .map_or(Callee::Local(realization.function), |owner| {
                        Callee::ClassStatic {
                            owner,
                            function: realization.function,
                        }
                    }),
                physical
                    .into_iter()
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| FirLoweringFailure::MissingLocalCallable(target.clone()))?,
            )
        };
        Ok(self.ir.add_expr(IrExpr::Call {
            callee,
            dispatch_receiver: None,
            args,
        }))
    }

    pub(super) fn checked_lambda(
        &mut self,
        callable: LocalCallableId,
        body: &FirBody,
        suspend: bool,
    ) -> Result<ExprId, FirLoweringFailure> {
        let (function, owner) = self.predeclare_local_function(body, callable)?;
        let unit_as_value = body
            .result_type()
            .is_some_and(|result| result.get() == Ty::Unit);
        if unit_as_value {
            self.ir.functions[function as usize].ret = Ty::obj("kotlin/Unit");
        }
        let realization = LocalCallableRealization {
            function,
            suspend,
            owner,
            source_name: body.debug_name().unwrap_or("<anonymous>").into(),
            captures: body.captures().to_vec(),
            implicit_receiver_captures: body.implicit_receiver_captures().to_vec(),
            context_parameter_count: body.context_receiver_types().len() as u32,
            has_extension_receiver: body.receiver_type().is_some(),
        };
        let previous = self
            .local_callable_scopes
            .last_mut()
            .expect("a body always has a local callable scope")
            .insert(callable, realization.clone());
        assert!(previous.is_none(), "a FIR lambda callable is declared once");
        let lowered = self.lower_nested_function(body, function, unit_as_value, true)?;
        if super::inline_returns::reachable_checked_returns(self.ir, lowered.callable)
            .iter()
            .any(|(_, depth)| *depth > 0)
        {
            // A return through an enclosing callable is meaningful only after this lambda's inline
            // template is spliced across that lexical boundary. Its standalone implementation has
            // the lambda's own return descriptor and therefore cannot represent the checked target.
            self.ir.inline_only_fns.insert(function);
        }
        self.ir.functions[function as usize].body = Some(lowered.callable);
        let captures = self.bound_all_captures(0, &realization)?;
        let own_parameters_from = u32::try_from(realization.capture_count()).map_err(|_| {
            FirLoweringFailure::MissingLocalCallable(FirLocalCallableRef {
                body_depth: 0,
                callable,
                declaration: None,
                external_capture_arguments: None,
            })
        })?;
        let previous = self
            .ir
            .lambda_own_params_from
            .insert(function, own_parameters_from);
        assert!(
            previous.is_none_or(|previous| previous == own_parameters_from),
            "one FIR lambda has one capture prefix"
        );
        let source_arity = self.ir.functions[function as usize]
            .params
            .len()
            .checked_sub(realization.capture_count())
            .ok_or(FirLoweringFailure::MissingLocalCallable(
                FirLocalCallableRef {
                    body_depth: 0,
                    callable,
                    declaration: None,
                    external_capture_arguments: None,
                },
            ))?;
        if suspend {
            self.ir.suspend_funs.push(function);
        }
        let arity = source_arity
            .checked_add(usize::from(suspend))
            .and_then(|arity| u8::try_from(arity).ok())
            .ok_or(FirLoweringFailure::MissingLocalCallable(
                FirLocalCallableRef {
                    body_depth: 0,
                    callable,
                    declaration: None,
                    external_capture_arguments: None,
                },
            ))?;
        Ok(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: function,
            arity,
            captures,
            sam: None,
            inline_body: Some(
                lowered
                    .inline
                    .expect("source-lambda lowering retains an inline template"),
            ),
        }))
    }

    pub(super) fn checked_local_callable_reference(
        &mut self,
        target: FirLocalCallableRef,
        extension_receiver: Option<crate::fir::FirReceiver>,
        adaptation: Option<&crate::fir::FirReferenceAdaptation>,
        reference_ty: Ty,
    ) -> Result<ExprId, FirLoweringFailure> {
        let (realization, external) = self.local_function(&target)?;
        let captures = if external {
            target
                .external_capture_arguments
                .as_deref()
                .ok_or_else(|| FirLoweringFailure::MissingLocalCallable(target.clone()))?
                .iter()
                .copied()
                .map(|argument| self.expression(argument))
                .collect::<Result<Vec<_>, _>>()?
        } else {
            self.bound_all_captures(target.body_depth, &realization)?
        };
        let extension_receiver = extension_receiver
            .map(|receiver| self.expression_with_conversion(receiver.value, receiver.conversion))
            .transpose()?;
        if let Ty::Fun(reference) = reference_ty.non_null() {
            if let Some(reference) = self.materialize_local_reference(
                &target,
                &realization,
                &captures,
                extension_receiver,
                adaptation,
                reference,
            )? {
                return Ok(reference);
            }
        }
        Err(FirLoweringFailure::UnsupportedLocalCallableReference(
            target,
        ))
    }

    fn materialize_local_reference(
        &mut self,
        target: &FirLocalCallableRef,
        realization: &LocalCallableRealization,
        bound_captures: &[ExprId],
        bound_extension_receiver: Option<ExprId>,
        adaptation: Option<&crate::fir::FirReferenceAdaptation>,
        reference: &crate::types::FnSig,
    ) -> Result<Option<ExprId>, FirLoweringFailure> {
        let reference_suspend = realization.suspend
            || adaptation.is_some_and(|adaptation| adaptation.suspend_conversion);
        let Some(arity) = u8::try_from(reference.params.len()).ok() else {
            return Ok(None);
        };
        if !realization.has_extension_receiver && bound_extension_receiver.is_some() {
            return Ok(None);
        }
        let capture_count = realization.capture_count();
        let logical_count = self.ir.functions[realization.function as usize]
            .params
            .len()
            .checked_sub(capture_count)
            .ok_or_else(|| FirLoweringFailure::MissingLocalCallable(target.clone()))?;
        let declaration_parameter_count = logical_count
            .checked_sub(usize::from(realization.has_extension_receiver))
            .ok_or_else(|| FirLoweringFailure::MissingLocalCallable(target.clone()))?;
        let bound_receiver_count = usize::from(bound_extension_receiver.is_some());
        let unbound_receiver_count =
            usize::from(realization.has_extension_receiver && bound_extension_receiver.is_none());
        if adaptation
            .is_some_and(|adaptation| adaptation.arguments.len() != declaration_parameter_count)
            || adaptation.is_none()
                && reference.params.len() != declaration_parameter_count + unbound_receiver_count
        {
            return Ok(None);
        }
        if unbound_receiver_count != 0 && reference.params.is_empty() {
            return Ok(None);
        }
        let own_start =
            u32::try_from(capture_count + bound_receiver_count + unbound_receiver_count)
                .map_err(|_| FirLoweringFailure::MissingLocalCallable(target.clone()))?;
        let identity_arguments;
        let adapted_arguments = if let Some(adaptation) = adaptation {
            adaptation.arguments.as_ref()
        } else {
            identity_arguments = (0..declaration_parameter_count)
                .map(|source| {
                    crate::fir::FirAdaptedReferenceArgument::Value(
                        u32::try_from(source).expect("too many local reference parameters"),
                    )
                })
                .collect::<Vec<_>>();
            &identity_arguments
        };
        let mut arguments = Vec::with_capacity(adapted_arguments.len());
        for (parameter, argument) in adapted_arguments.iter().enumerate() {
            let parameter = u32::try_from(parameter).expect("too many local parameters");
            arguments.push(match argument {
                crate::fir::FirAdaptedReferenceArgument::Value(source) => {
                    if reference.params.get(*source as usize).is_none() {
                        return Ok(None);
                    }
                    crate::ir::IrCheckedArgument::Expression {
                        parameter,
                        value: self.ir.add_expr(IrExpr::GetValue(
                            own_start
                                .checked_add(*source)
                                .expect("adapted reference parameter overflow"),
                        )),
                    }
                }
                crate::fir::FirAdaptedReferenceArgument::Default => {
                    crate::ir::IrCheckedArgument::Default { parameter }
                }
                crate::fir::FirAdaptedReferenceArgument::Vararg {
                    values,
                    whole_array,
                } => {
                    let mut elements = Vec::with_capacity(values.len());
                    for source in values.iter().copied() {
                        if reference.params.get(source as usize).is_none() {
                            return Ok(None);
                        }
                        elements.push((
                            self.ir.add_expr(IrExpr::GetValue(
                                own_start
                                    .checked_add(source)
                                    .expect("adapted reference parameter overflow"),
                            )),
                            *whole_array,
                        ));
                    }
                    crate::ir::IrCheckedArgument::Vararg {
                        parameter,
                        array_type: self.ir.functions[realization.function as usize].params
                            [capture_count + parameter as usize],
                        elements,
                    }
                }
            });
        }
        let wrapper_captures = (0..capture_count)
            .map(|slot| {
                self.ir.add_expr(IrExpr::GetValue(
                    u32::try_from(slot).expect("too many local captures"),
                ))
            })
            .collect::<Vec<_>>();
        let wrapper_extension_receiver = if realization.has_extension_receiver {
            Some(self.ir.add_expr(IrExpr::GetValue(
                u32::try_from(capture_count).expect("too many local captures"),
            )))
        } else {
            None
        };
        let call = self.materialize_local_call(
            target,
            realization,
            wrapper_captures,
            wrapper_extension_receiver,
            &arguments,
        )?;
        let selected_result = self.ir.functions[realization.function as usize].ret;
        let body = self.callable_reference_adapter_body(call, selected_result, reference.ret);
        let mut parameters = realization
            .captures
            .iter()
            .map(|capture| capture.ty.get())
            .chain(
                realization
                    .implicit_receiver_captures
                    .iter()
                    .map(|capture| capture.ty.get()),
            )
            .collect::<Vec<_>>();
        if bound_extension_receiver.is_some() {
            parameters.push(
                self.ir.functions[realization.function as usize].params
                    [capture_count + realization.context_parameter_count as usize],
            );
        }
        parameters.extend(reference.params.iter().copied());
        let wrapper = self.ir.add_fun(IrFunction {
            name: format!(
                "$fir_local_ref_{}_{}",
                self.body.owner().raw(),
                self.ir.functions.len()
            ),
            param_checks: vec![None; parameters.len()],
            params: parameters,
            ret: crate::types::stored_value_ty(reference.ret),
            body: Some(body),
            is_static: true,
            dispatch_receiver: None,
        });
        for (capture, captured) in realization.captures.iter().enumerate() {
            if captured.shared_cell {
                self.ir.shared_capture_parameters.insert(
                    (
                        wrapper,
                        u32::try_from(capture).expect("too many local reference captures"),
                    ),
                    captured.ty.get(),
                );
            }
        }
        if reference_suspend {
            self.ir.suspend_funs.push(wrapper);
        }
        self.ir.private_methods.insert(wrapper);
        self.ir.lambda_own_params_from.insert(
            wrapper,
            u32::try_from(capture_count + bound_receiver_count)
                .expect("too many local reference captures"),
        );
        let mut captures = bound_captures.to_vec();
        captures.extend(bound_extension_receiver);
        // FunctionReferenceImpl stores one bound receiver. A capture-free local reference is a
        // singleton; a reference with one lexical capture stores that capture as the bound receiver
        // and invokes the generated static adapter. Keep the lambda fallback only for the currently
        // unrepresentable multi-capture carrier shape; it remains executable while the IR grows an
        // explicit aggregate-capture reference representation.
        if captures.len() > 1 {
            return Ok(Some(self.ir.add_expr(IrExpr::Lambda {
                impl_fn: wrapper,
                arity,
                captures,
                sam: None,
                inline_body: None,
            })));
        }
        let bound = !captures.is_empty();
        let simple_name = format!(
            "$fir$local$fnref${}_{}",
            self.body.owner().raw(),
            self.ir.classes.len()
        );
        let internal = self.ir.package.as_ref().map_or_else(
            || type_name(&simple_name),
            |package| type_name(&format!("{}/{}", package.replace('.', "/"), simple_name)),
        );
        let mut class = IrClass::synthetic(internal);
        class.superclass = type_name("kotlin/jvm/internal/FunctionReferenceImpl");
        let continuation = Ty::obj("kotlin/coroutines/Continuation");
        let mut invoke_parameters = reference.params.clone();
        let mut target_parameters = self.ir.functions[wrapper as usize].params.clone();
        let mut invoke_result = reference.ret;
        let mut target_result = self.ir.functions[wrapper as usize].ret;
        if reference_suspend {
            invoke_parameters.push(continuation);
            target_parameters.push(continuation);
            invoke_result = Ty::obj("kotlin/Any");
            target_result = Ty::obj("kotlin/Any");
        }
        let declaration_suspend = realization.suspend;
        let mut reflection_parameters = reference.params.clone();
        let mut reflection_result = selected_result;
        if declaration_suspend {
            reflection_parameters.push(continuation);
            reflection_result = Ty::obj("kotlin/Any");
        }
        class.func_ref = Some(FuncRef {
            adapted: adaptation.is_some(),
            bound,
            arity,
            is_suspend: reference_suspend,
            module_target: None,
            local_target: Some(wrapper),
            owner_class: realization.owner,
            fn_name: realization.source_name.to_string(),
            flags: i32::from(realization.owner.is_none()),
            dispatch: if bound {
                FrDispatch::StaticBound
            } else {
                FrDispatch::Static
            },
            call_owner: None,
            call_name: self.ir.functions[wrapper as usize].name.clone(),
            reflection_name: None,
            reflection_receiver_parameter: false,
            reflection_target_ret_ty: Some(reflection_result),
            reflection_target_param_tys: Some(reflection_parameters),
            call_interface: false,
            param_tys: invoke_parameters,
            ret_ty: invoke_result,
            target_param_tys: target_parameters,
            target_ret_ty: target_result,
            unbox_params: vec![None; arity as usize],
            unbox_param_nullable: vec![false; arity as usize],
            box_ret: None,
            staticbound_recv_unbox: None,
        });
        let class = self.ir.add_class(class);
        Ok(Some(match captures.as_slice() {
            [capture] => self.ir.add_expr(IrExpr::New {
                internal,
                args: vec![*capture],
                ctor_params: Some(vec![Ty::obj("kotlin/Any")]),
                ctor_desc: None,
                external_target: None,
                defaults: Box::new([]),
                default_prefix_count: 0,
            }),
            [] => self.ir.add_expr(IrExpr::StaticInstance {
                owner: class,
                ty: class,
                field: "INSTANCE",
            }),
            _ => unreachable!("multi-capture references returned above"),
        }))
    }

    pub(super) fn local_function(
        &self,
        target: &FirLocalCallableRef,
    ) -> Result<(LocalCallableRealization, bool), FirLoweringFailure> {
        let depth = usize::try_from(target.body_depth).expect("local callable depth fits usize");
        let scope = self
            .local_callable_scopes
            .len()
            .checked_sub(depth + 1)
            .and_then(|index| self.local_callable_scopes.get(index));
        if let Some(realization) = scope.and_then(|scope| scope.get(&target.callable)).cloned() {
            return Ok((realization, false));
        }
        target
            .declaration
            .and_then(|declaration| self.published_local_callables.get(&declaration))
            .cloned()
            .map(|realization| (realization, true))
            .ok_or_else(|| FirLoweringFailure::MissingLocalCallable(target.clone()))
    }

    pub(super) fn bound_captures(
        &mut self,
        callable_depth: u32,
        captures: &[FirCapture],
    ) -> Result<Vec<ExprId>, FirLoweringFailure> {
        captures
            .iter()
            .map(|capture| {
                let depth = callable_depth
                    .checked_add(capture.enclosing_depth)
                    .expect("capture depth overflow");
                let slot = if depth == 0 {
                    self.value_slot(capture.source)
                } else {
                    self.capture_slots
                        .get(&(depth - 1, capture.source))
                        .map(|capture| capture.slot)
                        .ok_or(FirLoweringFailure::MissingCapture {
                            enclosing_depth: depth - 1,
                            source: capture.source,
                        })?
                };
                Ok(self.ir.add_expr(IrExpr::GetValue(slot)))
            })
            .collect()
    }

    fn bound_implicit_receiver_captures(
        &mut self,
        callable_depth: u32,
        captures: &[FirImplicitReceiverCapture],
    ) -> Result<Vec<ExprId>, FirLoweringFailure> {
        captures
            .iter()
            .map(|capture| {
                let depth = callable_depth
                    .checked_add(capture.enclosing_depth)
                    .expect("implicit receiver capture depth overflow");
                if depth == 0 && !capture.path.is_empty() {
                    return self.enclosing_receiver(&capture.path, capture.origin);
                }
                let slot = if depth == 0 {
                    self.implicit_receiver_slot(capture.current, capture.depth)
                } else {
                    self.implicit_receiver_capture_slot(
                        depth - 1,
                        capture.current,
                        capture.depth,
                        &capture.path,
                    )
                }
                .ok_or(FirLoweringFailure::MissingImplicitReceiver {
                    origin: capture.origin,
                })?;
                Ok(self.ir.add_expr(IrExpr::GetValue(slot)))
            })
            .collect()
    }

    fn bound_all_captures(
        &mut self,
        callable_depth: u32,
        realization: &LocalCallableRealization,
    ) -> Result<Vec<ExprId>, FirLoweringFailure> {
        let mut captures = self.bound_captures(callable_depth, &realization.captures)?;
        captures.extend(self.bound_implicit_receiver_captures(
            callable_depth,
            &realization.implicit_receiver_captures,
        )?);
        Ok(captures)
    }

    fn predeclare_local_function(
        &mut self,
        body: &FirBody,
        callable: LocalCallableId,
    ) -> Result<(crate::ir::FunId, Option<crate::types::TypeName>), FirLoweringFailure> {
        let result = body
            .result_type()
            .ok_or(FirLoweringFailure::MissingBodyResult {
                origin: body_origin(body),
            })?
            .get();
        let params = local_function_parameters(body);
        let mut param_checks = vec![None; params.len()];
        if body.is_source_lambda()
            && body.receiver_type().is_some_and(|receiver| {
                receiver.get().is_reference() && !receiver.get().is_nullable()
            })
        {
            let receiver = body.captures().len()
                + body.implicit_receiver_captures().len()
                + body.context_receiver_types().len();
            param_checks[receiver] = Some("<this>".to_owned());
        }
        let source_name = body.debug_name().unwrap_or("$fir_lambda");
        let name = format!(
            "{source_name}$fir_{}_{}_{}",
            body.owner().raw(),
            callable.raw(),
            self.ir.functions.len()
        );
        let function = self.ir.add_fun(IrFunction {
            name,
            param_checks,
            params,
            ret: result,
            body: None,
            is_static: true,
            dispatch_receiver: None,
        });
        self.ir.private_methods.insert(function);
        let owner = if let Some(owner) = body.lexical_class_owner() {
            let class = self
                .ir
                .checked_classifier_classes
                .get(&owner)
                .or_else(|| self.ir.checked_enum_entry_classes.get(&owner))
                .copied();
            if let Some(class) = class {
                self.ir.classes[class as usize].methods.push(function);
                self.ir.class_static_local_functions.insert(function);
                Some(self.ir.classes[class as usize].fq_name_id())
            } else {
                let declaration = crate::fir::DeclarationId::from_raw(body.owner().raw());
                let foreign_inline_template = self
                    .index
                    .callable_for_declaration(declaration)
                    .and_then(|callable| {
                        self.ir
                            .checked_callable_functions
                            .get(&callable.id)
                            .copied()
                    })
                    .is_some_and(|function| self.ir.foreign_inline_templates.contains(&function));
                if !foreign_inline_template {
                    return Err(FirLoweringFailure::MissingLocalClass(owner));
                }
                // The lexical class belongs to a different source file and is intentionally not
                // emitted in this IR file. This implementation exists only inside an inline
                // template; after splicing, the backend's ordinary lambda-reparenting pass assigns
                // it to the class that actually emits the call site.
                None
            }
        } else {
            None
        };
        for (parameter, capture) in body.captures().iter().enumerate() {
            if capture.shared_cell {
                self.ir.shared_capture_parameters.insert(
                    (
                        function,
                        u32::try_from(parameter).expect("too many lifted capture parameters"),
                    ),
                    capture.ty.get(),
                );
            }
        }
        let parameter_names = local_function_debug_parameter_names(self.body, body);
        self.ir
            .fn_params
            .entry(function)
            .or_insert_with(|| crate::ir::FnParamInfo::names(parameter_names));
        if let Some(line) = local_function_debug_line(body) {
            self.ir.fn_decl_lines.insert(function, line);
        }
        if body.is_source_lambda() {
            // An empty enclosing segment is the semantic class-initialization context. Do not put
            // the diagnostic placeholder `<anonymous>` into common IR: angle-bracket names are
            // illegal JVM methods, and other targets own their own physical spelling.
            let enclosing_name = body.debug_name().unwrap_or_default().to_owned();
            let binding_name = body.debug_binding_name().map(str::to_owned);
            // A local binding contributes to the lambda CLASS name, not to its implementation
            // method. Inside `init { val x = { ... } }`, for example, the class is `C$x$1` while
            // the method remains in the class-initializer sequence.
            let implementation_name = body.debug_name().unwrap_or_default().to_owned();
            let rendered_prefix = [Some(enclosing_name.as_str()), binding_name.as_deref()]
                .into_iter()
                .flatten()
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
                .join("$");
            let next = |select: &dyn Fn(&crate::ir::IrLambdaOrigin) -> Option<u32>| {
                self.ir
                    .lambda_origins
                    .values()
                    .filter_map(select)
                    .max()
                    .map_or(0, |ordinal| ordinal.saturating_add(1))
            };
            let identity = next(&|origin| Some(origin.identity));
            let implementation_ordinal = next(&|origin| {
                (origin.lexical_owner == owner && origin.implementation_name == implementation_name)
                    .then_some(origin.implementation_ordinal)
            });
            let ordinal = next(&|origin| {
                let origin_prefix = [
                    Some(origin.enclosing_name.as_str()),
                    origin.binding_name.as_deref(),
                ]
                .into_iter()
                .flatten()
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
                .join("$");
                (origin.lexical_owner == owner && origin_prefix == rendered_prefix)
                    .then_some(origin.ordinal)
            });
            let previous = self.ir.lambda_origins.insert(
                function,
                crate::ir::IrLambdaOrigin {
                    identity,
                    lexical_owner: owner,
                    enclosing_name,
                    binding_name,
                    ordinal,
                    implementation_name,
                    implementation_ordinal,
                },
            );
            assert!(previous.is_none(), "one FIR lambda has one semantic origin");
        }
        Ok((function, owner))
    }

    fn lower_nested_function(
        &mut self,
        body: &FirBody,
        function: crate::ir::FunId,
        unit_as_value: bool,
        retain_inline_template: bool,
    ) -> Result<NestedCallableBodies, FirLoweringFailure> {
        #[cfg(feature = "trace")]
        super::trace_checked_body(body, self.index);
        let capture_slots = body
            .captures()
            .iter()
            .enumerate()
            .map(|(slot, capture)| {
                (
                    (capture.enclosing_depth, capture.source),
                    CaptureSlot {
                        slot: u32::try_from(slot).expect("too many FIR captures"),
                        ty: capture.ty,
                        shared_cell: capture.shared_cell,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let mut scopes = self.local_callable_scopes.clone();
        scopes.push(HashMap::new());
        let mut nested = BodyLowering::new(
            body,
            self.index,
            self.ir,
            false,
            capture_slots,
            scopes,
            self.published_local_callables.clone(),
        );
        nested.control_path = self.control_path.clone();
        nested.control_path.push(
            body.local_callable()
                .ok_or(FirLoweringFailure::MissingBodyLocalCallable(body.owner()))?,
        );
        nested.prepare_local_functions()?;
        nested.realize_local_functions()?;
        let mut defaults = vec![None; local_function_parameters(body).len()];
        for default in body.default_values() {
            let mut position = body.captures().len()
                + body.implicit_receiver_captures().len()
                + default.parameter as usize;
            if body.receiver_type().is_some() && default.parameter >= body.context_value_count() {
                position += 1;
            }
            let Some(slot) = defaults.get_mut(position) else {
                return Err(FirLoweringFailure::MissingLocalDefault {
                    function,
                    parameter: default.parameter,
                });
            };
            *slot = Some(nested.expression(default.value)?);
        }
        if defaults.iter().any(Option::is_some) {
            nested.ir.fn_params.insert(
                function,
                crate::ir::FnParamInfo::defaults(Vec::new(), defaults),
            );
        }
        let roots = body
            .roots()
            .iter()
            .copied()
            .map(|root| nested.statement(root))
            .collect::<Result<Vec<_>, _>>()?;
        let result = body
            .result_type()
            .ok_or(FirLoweringFailure::MissingBodyResult {
                origin: body_origin(body),
            })?
            .get();
        let inline = if retain_inline_template {
            let inline =
                inline_callable_body(nested.ir, &roots, result, body.has_implicit_return());
            let (inline, _) = crate::ir::clone_expression_dag(nested.ir, inline);
            Some(
                super::inline_returns::prepare_inline_template(
                    nested.ir,
                    inline,
                    result,
                    true,
                    &mut nested.next_temporary,
                )
                .ok_or(FirLoweringFailure::MissingBodyResult {
                    origin: body_origin(body),
                })?,
            )
        } else {
            None
        };
        let callable = finish_callable_body(
            nested.ir,
            roots,
            result,
            body.has_implicit_return(),
            unit_as_value,
            body_origin(body),
        )?;
        let published_local_callables = std::mem::take(&mut nested.published_local_callables);
        drop(nested);
        self.published_local_callables = published_local_callables;
        Ok(NestedCallableBodies { callable, inline })
    }
}

#[derive(Clone, Copy)]
struct NestedCallableBodies {
    callable: ExprId,
    inline: Option<ExprId>,
}

/// Build the value-producing form consumed by declaration-defined inline expansion. Unlike a
/// callable method body, this form has no synthetic return: source returns remain control-flow
/// nodes, while an implicit result remains the block value at the call site.
fn inline_callable_body(
    ir: &mut crate::ir::IrFile,
    roots: &[ExprId],
    result: Ty,
    implicit_return: bool,
) -> ExprId {
    if !implicit_return {
        return ir.add_expr(IrExpr::Block {
            stmts: roots.to_vec(),
            value: None,
        });
    }
    if result == Ty::Unit {
        let unit = ir.add_expr(IrExpr::UnitInstance);
        return ir.add_expr(IrExpr::Block {
            stmts: roots.to_vec(),
            value: Some(unit),
        });
    }
    let mut statements = roots.to_vec();
    let value = statements
        .pop()
        .expect("checked implicit non-Unit body has a result expression");
    if statements.is_empty() {
        value
    } else {
        ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: Some(value),
        })
    }
}

fn local_function_parameters(body: &FirBody) -> Vec<Ty> {
    body.captures()
        .iter()
        .map(|capture| capture.ty.get())
        .chain(
            body.implicit_receiver_captures()
                .iter()
                .map(|capture| capture.ty.get()),
        )
        .chain(
            body.context_receiver_types()
                .iter()
                .map(|receiver| receiver.get()),
        )
        .chain(body.receiver_type().map(|receiver| receiver.get()))
        .chain(
            body.parameters()
                .iter()
                .skip(body.context_value_count() as usize)
                .map(|parameter| parameter.ty.get()),
        )
        .collect()
}

fn local_function_debug_parameter_names(enclosing: &FirBody, body: &FirBody) -> Vec<String> {
    let mut names = body
        .captures()
        .iter()
        .enumerate()
        .map(|(ordinal, capture)| {
            (capture.enclosing_depth == 0)
                .then(|| enclosing.debug_value_name(capture.source))
                .flatten()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("$capture{ordinal}"))
        })
        .collect::<Vec<_>>();
    names.extend(
        body.implicit_receiver_captures()
            .iter()
            .enumerate()
            .map(|(ordinal, _)| format!("$this${ordinal}")),
    );
    let context_value_count = body.context_value_count() as usize;
    for ordinal in 0..body.context_receiver_types().len() {
        let name = (ordinal < context_value_count)
            .then(|| body.parameters().get(ordinal))
            .flatten()
            .and_then(|parameter| body.debug_value_name(parameter.value))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("$context_receiver_{ordinal}"));
        names.push(name);
    }
    if body.receiver_type().is_some() {
        names.push("<this>".to_owned());
    }
    names.extend(
        body.parameters()
            .iter()
            .skip(context_value_count)
            .enumerate()
            .map(|(ordinal, parameter)| {
                body.debug_value_name(parameter.value)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("p{ordinal}"))
            }),
    );
    names
}

fn local_function_debug_line(body: &FirBody) -> Option<u32> {
    let root = *body.roots().first()?;
    let statement_line = body.statement_debug_line(root);
    if statement_line != 0 {
        return Some(statement_line);
    }
    let crate::fir::FirStatementKind::Expression(expression) = &body.statement(root)?.kind else {
        return None;
    };
    let source_line = body.expression_debug_lines(*expression).source;
    (source_line != 0).then_some(source_line)
}

fn body_origin(body: &FirBody) -> crate::fir::OriginId {
    body.roots()
        .first()
        .and_then(|root| body.statement(*root))
        .map_or(crate::fir::OriginId::from_raw(0), |statement| {
            statement.origin
        })
}
