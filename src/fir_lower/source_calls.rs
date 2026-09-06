//! Realization of stable same-file callable identities as ordinary common-IR calls.

use crate::fir::{
    CallableId, DeclarationKind, ExternalCallableId, ExternalPropertyId, FirAnnotationConstruction,
    FirAnnotationDefaultValue, FirConstant, ResolvedTy,
};
use crate::ir::{Callee, ExprId, IrCheckedArgument, IrConst, IrExpr, IrTypeOp};
use crate::types::Ty;

use super::checked_arguments::{
    materialize_checked_arguments, CheckedArgumentSlot, CheckedArgumentValue,
};
use super::BodyLowering;

#[derive(Clone, Copy)]
enum CheckedArgumentPolicy<'a> {
    Selected {
        defaults: SelectedDefaultMode,
        preserve_inline_lambdas: bool,
    },
    SameFileInline {
        declared_parameters: &'a [Ty],
        inline: bool,
    },
}

struct NormalizedCheckedArguments {
    statements: Vec<ExprId>,
    slots: Vec<Option<ExprId>>,
    inline_lambdas: Vec<Option<ExprId>>,
    defaults: Vec<u32>,
}

#[derive(Clone, Copy)]
pub(super) enum SelectedOperandMode {
    DirectWhenOrdered,
    Materialized,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum SelectedDefaultMode {
    Reject,
    Omit,
    Materialize,
}

pub(super) struct SelectedOperandRequest<'a> {
    pub(super) receiver_ty: Option<ResolvedTy>,
    pub(super) parameter_types: &'a [Ty],
    pub(super) dispatch_receiver: Option<ExprId>,
    pub(super) extension_receiver: Option<ExprId>,
    pub(super) arguments: &'a [IrCheckedArgument],
    pub(super) defaults: SelectedDefaultMode,
    pub(super) preserve_inline_lambdas: bool,
    pub(super) extension_receiver_parameter: Option<u32>,
    pub(super) mode: SelectedOperandMode,
}

pub(super) struct ExternalCallRequest<'a> {
    pub(super) target: ExternalCallableId,
    pub(super) default_provider: Option<ExternalCallableId>,
    pub(super) receiver_ty: Option<ResolvedTy>,
    pub(super) declared_receiver: Option<ResolvedTy>,
    pub(super) parameters: &'a [ResolvedTy],
    pub(super) result: ResolvedTy,
    pub(super) declared_result: Option<ResolvedTy>,
    pub(super) suspend: bool,
    pub(super) can_inline: bool,
    pub(super) inline_plan: Option<&'a crate::fir::FirInlineBodyPlan>,
    pub(super) substitutions: &'a [crate::fir::FirTypeSubstitution],
    pub(super) extension_receiver_parameter: Option<u32>,
    pub(super) dispatch_receiver: Option<ExprId>,
    pub(super) extension_receiver: Option<ExprId>,
    pub(super) arguments: &'a [IrCheckedArgument],
}

pub(super) struct ModuleConstructorRequest<'a> {
    pub(super) target: CallableId,
    pub(super) classifier: crate::types::TypeName,
    pub(super) argument_parameter_types: &'a [Ty],
    pub(super) declaration_parameter_types: &'a [Ty],
    pub(super) primary_in_current_file: bool,
    pub(super) context_parameter_count: u32,
    pub(super) outer_receiver: Option<ExprId>,
    pub(super) external_capture_arguments: Option<&'a [(ExprId, Ty)]>,
    pub(super) arguments: &'a [IrCheckedArgument],
}

struct ExternalInlineCallRequest<'a> {
    plan: &'a crate::fir::FirInlineBodyPlan,
    receiver_ty: Option<ResolvedTy>,
    parameter_types: &'a [Ty],
    result: ResolvedTy,
    dispatch_receiver: Option<ExprId>,
    extension_receiver: Option<ExprId>,
    arguments: &'a [IrCheckedArgument],
}

/// Whether the checked source-order operand stream is already in selected parameter order. Missing
/// defaults have no evaluation and therefore do not disturb the order; repeated vararg fragments
/// remain adjacent at one parameter and are grouped without reordering their elements.
fn arguments_follow_parameter_order(
    arguments: &[IrCheckedArgument],
    preceding_parameter: Option<u32>,
) -> bool {
    let mut previous = preceding_parameter;
    for argument in arguments {
        let parameter = match argument {
            IrCheckedArgument::Expression { parameter, .. } => *parameter,
            IrCheckedArgument::Vararg {
                parameter,
                elements,
                ..
            } if !elements.is_empty() => *parameter,
            IrCheckedArgument::Default { .. } | IrCheckedArgument::Vararg { .. } => continue,
        };
        if previous.is_some_and(|previous| parameter < previous) {
            return false;
        }
        previous = Some(parameter);
    }
    true
}

impl BodyLowering<'_> {
    /// Whether a checked common-IR operand contains a suspension that belongs to the current body.
    /// The checker/lowering maps are authoritative; this performs no callable lookup. A lambda body
    /// remains a separate semantic body, while evaluating its captures still belongs to the caller.
    fn operand_suspends(&self, expression: ExprId) -> bool {
        if self.ir.suspend_calls.contains_key(&expression) {
            return true;
        }
        match self.ir.expr(expression) {
            IrExpr::Call {
                callee: Callee::Local(function),
                ..
            }
            | IrExpr::Call {
                callee: Callee::ClassStatic { function, .. },
                ..
            } => {
                if self.ir.suspend_funs.contains(function) {
                    return true;
                }
            }
            IrExpr::MethodCall { class, index, .. } => {
                if self.ir.classes[*class as usize]
                    .methods
                    .get(*index as usize)
                    .is_some_and(|function| self.ir.suspend_funs.contains(function))
                {
                    return true;
                }
            }
            IrExpr::Lambda { captures, .. } => {
                return captures
                    .iter()
                    .any(|capture| self.operand_suspends(*capture));
            }
            _ => {}
        }
        let mut suspends = false;
        crate::ir::for_each_child(&self.ir.exprs, expression, &mut |child| {
            if !suspends && self.operand_suspends(child) {
                suspends = true;
            }
        });
        suspends
    }

    fn checked_operands_suspend(
        &self,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
    ) -> bool {
        dispatch_receiver
            .into_iter()
            .chain(extension_receiver)
            .any(|operand| self.operand_suspends(operand))
            || arguments.iter().any(|argument| match argument {
                IrCheckedArgument::Expression { value, .. } => self.operand_suspends(*value),
                IrCheckedArgument::Vararg { elements, .. } => elements
                    .iter()
                    .any(|(value, _)| self.operand_suspends(*value)),
                IrCheckedArgument::Default { .. } => false,
            })
    }

    /// Inline the already-checked block passed to one of Kotlin's coroutine primitives.
    ///
    /// FIR has selected the intrinsic declaration and its sole lambda argument. Common lowering
    /// therefore performs no lookup: it invokes that exact retained lambda body with the semantic
    /// current-continuation placeholder, reusing the ordinary inline-lambda splicer for captures and
    /// local-value rebasing. The resulting structural block is recorded as one atomic suspension
    /// point for target coroutine realization.
    pub(super) fn suspend_coroutine_primitive(
        &mut self,
        operation: &crate::fir::FirIntrinsic,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[crate::fir::FirCallArgument],
        result: ResolvedTy,
    ) -> Option<ExprId> {
        if dispatch_receiver.is_some() || extension_receiver.is_some() {
            return None;
        }
        let [crate::fir::FirCallArgument::Expression {
            parameter: 0,
            value,
            conversion,
        }] = arguments
        else {
            return None;
        };
        let lambda = self.expression_with_conversion(*value, *conversion).ok()?;
        let (implementation, capture_count, arity) = match self.ir.expr(lambda) {
            IrExpr::Lambda {
                impl_fn,
                captures,
                arity,
                inline_body: Some(_),
                ..
            } => (*impl_fn, captures.len(), *arity),
            _ => return None,
        };
        if arity != 1 {
            return None;
        }
        let continuation_ty = *self
            .ir
            .functions
            .get(implementation as usize)?
            .params
            .get(capture_count)?;
        let continuation = self.ir.add_expr(IrExpr::CurrentContinuation);
        let invocation = self.ir.add_expr(IrExpr::InvokeFunction {
            func: lambda,
            args: vec![continuation],
            params: vec![continuation_ty],
            ret: self.ir.functions.get(implementation as usize)?.ret,
        });
        self.splice_inline_lambda_invocation(invocation)?;
        let kind = match operation {
            crate::fir::FirIntrinsic::SuspendCoroutine => {
                crate::ir::IrIntrinsicSuspensionKind::Safe
            }
            crate::fir::FirIntrinsic::SuspendCoroutineUninterceptedOrReturn => {
                crate::ir::IrIntrinsicSuspensionKind::Unintercepted
            }
            _ => return None,
        };
        self.ir.intrinsic_suspension_points.insert(
            invocation,
            crate::ir::IrIntrinsicSuspensionPoint {
                result: result.get(),
                kind,
            },
        );
        Some(invocation)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn external_property_access(
        &mut self,
        target: ExternalPropertyId,
        dispatch: crate::fir::FirPropertyDispatch,
        receiver_ty: Option<ResolvedTy>,
        parameters: &[ResolvedTy],
        result: ResolvedTy,
        extension_receiver_parameter: Option<u32>,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
        write: bool,
    ) -> Option<ExprId> {
        let source_receiver = dispatch_receiver
            .is_some()
            .then_some(receiver_ty)
            .flatten()
            .map(ResolvedTy::get);
        let parameter_types = parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        let (statements, receiver, arguments, defaults) =
            self.selected_semantic_operands(SelectedOperandRequest {
                receiver_ty,
                parameter_types: &parameter_types,
                dispatch_receiver,
                extension_receiver,
                arguments,
                defaults: SelectedDefaultMode::Reject,
                preserve_inline_lambdas: false,
                extension_receiver_parameter,
                mode: SelectedOperandMode::DirectWhenOrdered,
            })?;
        debug_assert!(defaults.is_empty());
        let operation = if write {
            crate::ir::IrCheckedOperation::ExternalPropertyWrite {
                target,
                dispatch: match dispatch {
                    crate::fir::FirPropertyDispatch::Ordinary => {
                        crate::ir::IrPropertyDispatch::Ordinary
                    }
                    crate::fir::FirPropertyDispatch::Super { owner, interface } => {
                        crate::ir::IrPropertyDispatch::Super { owner, interface }
                    }
                },
                receiver,
                arguments,
                parameters: parameter_types,
                result: result.get(),
                source_receiver,
            }
        } else {
            crate::ir::IrCheckedOperation::ExternalPropertyRead {
                target,
                dispatch: match dispatch {
                    crate::fir::FirPropertyDispatch::Ordinary => {
                        crate::ir::IrPropertyDispatch::Ordinary
                    }
                    crate::fir::FirPropertyDispatch::Super { owner, interface } => {
                        crate::ir::IrPropertyDispatch::Super { owner, interface }
                    }
                },
                receiver,
                arguments,
                parameters: parameter_types,
                result: result.get(),
                source_receiver,
            }
        };
        let access = self.ir.add_expr(IrExpr::Checked(operation));
        Some(self.wrap_call_statements(statements, access))
    }

    /// Materialize a checked dependency call without exposing its physical owner or descriptor.
    /// Argument ordinals were selected by the checker; this routine only preserves source
    /// evaluation order and lays the already-mapped values into semantic parameter order.
    pub(super) fn external_call(&mut self, request: ExternalCallRequest<'_>) -> Option<ExprId> {
        let ExternalCallRequest {
            target,
            default_provider,
            receiver_ty,
            declared_receiver,
            parameters,
            result,
            declared_result,
            suspend,
            can_inline,
            inline_plan,
            substitutions,
            extension_receiver_parameter,
            dispatch_receiver,
            extension_receiver,
            arguments,
        } = request;
        // Preserve the checked SOURCE receiver across dependency realization. An extension records
        // its declaration receiver (a generic `T` deliberately records nothing); a member records
        // its selected dispatch type. The JVM realization may turn either into argument zero of a
        // static method, at which point the descriptor alone cannot distinguish a value-class
        // carrier from an ordinary erased `Object` parameter.
        let source_receiver = declared_receiver
            .or_else(|| dispatch_receiver.is_some().then_some(receiver_ty).flatten());
        let parameter_types = parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        if let Some(plan) = inline_plan {
            if let Some(expanded) = self.external_inline_call(ExternalInlineCallRequest {
                plan,
                receiver_ty,
                parameter_types: &parameter_types,
                result,
                dispatch_receiver,
                extension_receiver,
                arguments,
            }) {
                self.ir.inline_regions.insert(expanded);
                return Some(expanded);
            }
        }
        let (statements, receiver, args, mut defaults) =
            self.selected_semantic_operands(SelectedOperandRequest {
                receiver_ty,
                parameter_types: &parameter_types,
                dispatch_receiver,
                extension_receiver,
                arguments,
                defaults: SelectedDefaultMode::Omit,
                preserve_inline_lambdas: can_inline,
                extension_receiver_parameter,
                mode: SelectedOperandMode::DirectWhenOrdered,
            })?;
        // The external FIR target temporarily inserts a MEMBER EXTENSION receiver into its parameter
        // vector so every operand has one checked slot. It is still a receiver, not a source value
        // parameter, and Kotlin default-mask ordinals count only value parameters. Publish the
        // backend-neutral semantic ordinals on common IR before the target realizes its bridge.
        if let Some(extension_parameter) = extension_receiver_parameter {
            for default in &mut defaults {
                if *default == extension_parameter {
                    return None;
                }
                if *default > extension_parameter {
                    *default -= 1;
                }
            }
        }
        let call = self.ir.add_expr(IrExpr::Call {
            callee: Callee::External {
                target,
                default_provider,
                params: parameter_types,
                ret: result.get(),
                substitutions: super::checked::lower_substitutions(substitutions),
                defaults,
                extension_receiver_parameter,
            },
            dispatch_receiver: receiver,
            args,
        });
        if let Some(receiver) = source_receiver {
            self.ir
                .ext_call_source_receiver
                .insert(call, receiver.get());
        }
        if let Some(result) = declared_result {
            self.ir.call_declared_ret.insert(call, result.get());
        }
        if can_inline {
            self.ir.inline_call_sites.insert(call);
        }
        if suspend {
            self.ir.suspend_calls.insert(call, result.get());
            crate::trace_compiler!(
                "fir",
                "published external suspend call expression={call} target={target:?} result={:?}",
                result.get(),
            );
        }
        let region = self.wrap_call_statements(statements, call);
        if can_inline {
            self.ir.inline_regions.insert(region);
        }
        Some(region)
    }

    fn external_inline_call(&mut self, request: ExternalInlineCallRequest<'_>) -> Option<ExprId> {
        let ExternalInlineCallRequest {
            plan,
            receiver_ty,
            parameter_types,
            result,
            dispatch_receiver,
            extension_receiver,
            arguments,
        } = request;
        let (lambda_parameter, invocation_arguments, returned_value) = match plan {
            crate::fir::FirInlineBodyPlan::ForEach {
                lambda_parameter,
                iterator_ty,
                iterator,
                has_next,
                next,
            } => {
                return self.external_inline_for_each(
                    *lambda_parameter,
                    *iterator_ty,
                    iterator,
                    has_next,
                    next,
                    receiver_ty,
                    parameter_types,
                    dispatch_receiver,
                    extension_receiver,
                    arguments,
                );
            }
            crate::fir::FirInlineBodyPlan::CollectionTransform {
                lambda_parameter,
                flatten,
                iterator_ty,
                iterator,
                has_next,
                next,
                factory,
                factory_classifier,
                append,
                accumulator,
                append_parameter,
                append_result,
            } => {
                return self.external_inline_collection_transform(
                    *lambda_parameter,
                    *flatten,
                    *iterator_ty,
                    iterator,
                    has_next,
                    next,
                    *factory,
                    *factory_classifier,
                    *append,
                    *accumulator,
                    *append_parameter,
                    *append_result,
                    receiver_ty,
                    parameter_types,
                    dispatch_receiver,
                    extension_receiver,
                    arguments,
                );
            }
            crate::fir::FirInlineBodyPlan::SuspendBeforeLambdaFinally {
                lambda_parameter,
                state_parameter,
                state_default,
                enter,
                cleanup,
            } => {
                return self.external_suspend_finally_inline_call(
                    *lambda_parameter,
                    *state_parameter,
                    *state_default,
                    enter,
                    cleanup,
                    receiver_ty,
                    parameter_types,
                    result,
                    dispatch_receiver,
                    extension_receiver,
                    arguments,
                );
            }
            crate::fir::FirInlineBodyPlan::InvokeLambda {
                lambda_parameter,
                arguments,
                result,
            } => (*lambda_parameter, arguments, *result),
        };
        let lambda_parameter = lambda_parameter as usize;
        let (mut statements, receiver, args, defaults) =
            self.selected_semantic_operands(SelectedOperandRequest {
                receiver_ty,
                parameter_types,
                dispatch_receiver,
                extension_receiver,
                arguments,
                defaults: SelectedDefaultMode::Reject,
                preserve_inline_lambdas: false,
                extension_receiver_parameter: None,
                mode: SelectedOperandMode::Materialized,
            })?;
        debug_assert!(defaults.is_empty());
        let invocation_operands = invocation_arguments
            .iter()
            .map(|operand| {
                Some(match operand {
                    crate::fir::FirInlineValue::Receiver => (receiver?, receiver_ty?.get()),
                    crate::fir::FirInlineValue::Parameter(parameter) => (
                        *args.get(*parameter as usize)?,
                        *parameter_types.get(*parameter as usize)?,
                    ),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let inline_body = self.materialize_external_inline_lambda(
            &mut statements,
            &args,
            lambda_parameter,
            &invocation_operands,
        )?;

        if let Some(returned_value) = returned_value {
            statements.push(inline_body);
            let value = match returned_value {
                crate::fir::FirInlineValue::Receiver => receiver?,
                crate::fir::FirInlineValue::Parameter(parameter) => {
                    *args.get(parameter as usize)?
                }
            };
            return Some(self.ir.add_expr(IrExpr::Block {
                stmts: statements,
                value: Some(value),
            }));
        }
        Some(if statements.is_empty() {
            inline_body
        } else {
            self.ir.add_expr(IrExpr::Block {
                stmts: statements,
                value: Some(inline_body),
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn external_suspend_finally_inline_call(
        &mut self,
        lambda_parameter: u32,
        state_parameter: u32,
        state_default: crate::fir::FirInlineDefaultValue,
        enter: &crate::fir::FirInlineMemberCall,
        cleanup: &crate::fir::FirInlineMemberCall,
        receiver_ty: Option<ResolvedTy>,
        parameter_types: &[Ty],
        result: ResolvedTy,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
    ) -> Option<ExprId> {
        let receiver_ty = receiver_ty?.get();
        let (mut statements, receiver, args, defaults) =
            self.selected_semantic_operands(SelectedOperandRequest {
                receiver_ty: ResolvedTy::new(receiver_ty).ok(),
                parameter_types,
                dispatch_receiver,
                extension_receiver,
                arguments,
                defaults: SelectedDefaultMode::Materialize,
                preserve_inline_lambdas: false,
                extension_receiver_parameter: None,
                mode: SelectedOperandMode::Materialized,
            })?;
        let receiver = receiver?;
        let state_parameter = state_parameter as usize;
        if defaults
            .iter()
            .any(|default| *default as usize != state_parameter)
        {
            return None;
        }
        let state_ty = *parameter_types.get(state_parameter)?;
        let state_value = if defaults
            .iter()
            .any(|default| *default as usize == state_parameter)
        {
            match state_default {
                crate::fir::FirInlineDefaultValue::Null => {
                    self.ir.add_expr(IrExpr::Const(IrConst::Null))
                }
            }
        } else {
            *args.get(state_parameter)?
        };
        let state_slot = self.allocate_temporary();
        statements.push(self.ir.add_expr(IrExpr::Variable {
            index: state_slot,
            ty: state_ty,
            init: Some(state_value),
            named: false,
        }));
        let inline_body = self.materialize_external_inline_lambda(
            &mut statements,
            &args,
            lambda_parameter as usize,
            &[],
        )?;

        let enter_state = self.ir.add_expr(IrExpr::GetValue(state_slot));
        let enter_call =
            self.external_inline_member_call(enter, receiver, receiver_ty, vec![enter_state])?;
        statements.push(enter_call);

        let result_ty = result.get();
        let result_slot = self.allocate_temporary();
        let initial = self
            .ir
            .add_expr(IrExpr::Const(IrConst::zero_for_value_type(result_ty)));
        statements.push(self.ir.add_expr(IrExpr::Variable {
            index: result_slot,
            ty: result_ty,
            init: Some(initial),
            named: false,
        }));
        let store_result = self.ir.add_expr(IrExpr::SetValue {
            var: result_slot,
            value: inline_body,
        });
        let try_body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![store_result],
            value: None,
        });
        let cleanup_state = self.ir.add_expr(IrExpr::GetValue(state_slot));
        let cleanup_call =
            self.external_inline_member_call(cleanup, receiver, receiver_ty, vec![cleanup_state])?;
        let finally = self.ir.add_expr(IrExpr::Block {
            stmts: vec![cleanup_call],
            value: None,
        });
        statements.push(self.ir.add_expr(IrExpr::Try {
            body: try_body,
            catches: Vec::new(),
            finally: Some(finally),
            result: Ty::Unit,
        }));
        let value = self.ir.add_expr(IrExpr::GetValue(result_slot));
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: Some(value),
        }))
    }

    fn external_inline_member_call(
        &mut self,
        plan: &crate::fir::FirInlineMemberCall,
        receiver: ExprId,
        receiver_ty: Ty,
        args: Vec<ExprId>,
    ) -> Option<ExprId> {
        if plan.parameters.len() != args.len() {
            return None;
        }
        let call = self.ir.add_expr(IrExpr::Call {
            callee: Callee::External {
                target: plan.declaration,
                default_provider: None,
                params: plan
                    .parameters
                    .iter()
                    .map(|parameter| parameter.get())
                    .collect(),
                ret: plan.result.get(),
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter: None,
            },
            dispatch_receiver: Some(receiver),
            args,
        });
        self.ir.ext_call_source_receiver.insert(call, receiver_ty);
        if plan.suspend {
            self.ir.suspend_calls.insert(call, plan.result.get());
        }
        Some(call)
    }

    /// Replace one already-evaluated lambda operand with its checked inline-body template. Capture
    /// evaluation stays at the lambda's source position, and every external inline shape shares this
    /// single local-slot rebasing path.
    fn materialize_external_inline_lambda(
        &mut self,
        statements: &mut Vec<ExprId>,
        args: &[ExprId],
        lambda_parameter: usize,
        invocation_operands: &[(ExprId, Ty)],
    ) -> Option<ExprId> {
        let lambda_slot = match self.ir.expr(*args.get(lambda_parameter)?) {
            IrExpr::GetValue(slot) => *slot,
            _ => return None,
        };
        let declaration_position = statements.iter().position(|statement| {
            matches!(
                self.ir.expr(*statement),
                IrExpr::Variable { index, .. } if *index == lambda_slot
            )
        })?;
        let lambda = match self.ir.expr(statements[declaration_position]).clone() {
            IrExpr::Variable {
                init: Some(lambda), ..
            } => lambda,
            _ => return None,
        };
        let (implementation, captures, inline_body, arity) = match self.ir.expr(lambda).clone() {
            IrExpr::Lambda {
                impl_fn,
                captures,
                inline_body: Some(inline_body),
                arity,
                ..
            } => (impl_fn, captures, inline_body, arity as usize),
            _ => return None,
        };
        if invocation_operands.len() != arity {
            return None;
        }

        statements.remove(declaration_position);
        let mut capture_declarations = Vec::with_capacity(captures.len());
        let mut formal_slots = Vec::with_capacity(captures.len() + arity);
        for (capture_ordinal, capture) in captures.into_iter().enumerate() {
            if self.ir.shared_capture_parameters.contains_key(&(
                implementation,
                u32::try_from(capture_ordinal).expect("too many inline captures"),
            )) {
                let IrExpr::GetValue(slot) = self.ir.expr(capture) else {
                    return None;
                };
                formal_slots.push(*slot);
                continue;
            }
            let slot = self.allocate_temporary();
            let ty = *self
                .ir
                .functions
                .get(implementation as usize)?
                .params
                .get(capture_ordinal)?;
            capture_declarations.push(self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty,
                init: Some(capture),
                named: false,
            }));
            formal_slots.push(slot);
        }
        statements.splice(
            declaration_position..declaration_position,
            capture_declarations,
        );
        for &(value, ty) in invocation_operands {
            let slot = match self.ir.expr(value) {
                IrExpr::GetValue(slot) => *slot,
                _ => {
                    let slot = self.allocate_temporary();
                    statements.push(self.ir.add_expr(IrExpr::Variable {
                        index: slot,
                        ty,
                        init: Some(value),
                        named: false,
                    }));
                    slot
                }
            };
            formal_slots.push(slot);
        }

        let local_base = self.next_temporary;
        let local_count =
            rehome_inline_body_values(self.ir, inline_body, &formal_slots, local_base)?;
        self.next_temporary = local_base.checked_add(local_count)?;
        self.ir.functions[implementation as usize].body = None;
        self.ir.inline_only_fns.insert(implementation);
        Some(inline_body)
    }

    /// Expand the checked structural body of an exact collection `map`/`flatMap` declaration only
    /// when its inline lambda contains a suspension. Ordinary calls retain the library invocation;
    /// a suspending body must join the enclosing function before target coroutine lowering.
    #[allow(clippy::too_many_arguments)]
    fn external_inline_collection_transform(
        &mut self,
        lambda_parameter: u32,
        flatten: bool,
        iterator_ty: ResolvedTy,
        iterator: &crate::fir::FirIteratorCall,
        has_next: &crate::fir::FirIteratorCall,
        next: &crate::fir::FirIteratorCall,
        factory: ExternalCallableId,
        factory_classifier: crate::types::TypeName,
        append: ExternalCallableId,
        accumulator_ty: ResolvedTy,
        append_parameter: ResolvedTy,
        append_result: ResolvedTy,
        receiver_ty: Option<ResolvedTy>,
        parameter_types: &[Ty],
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
    ) -> Option<ExprId> {
        let (mut statements, receiver, args, defaults) =
            self.selected_semantic_operands(SelectedOperandRequest {
                receiver_ty,
                parameter_types,
                dispatch_receiver,
                extension_receiver,
                arguments,
                defaults: SelectedDefaultMode::Reject,
                preserve_inline_lambdas: false,
                extension_receiver_parameter: None,
                mode: SelectedOperandMode::Materialized,
            })?;
        debug_assert!(defaults.is_empty());
        let iterable = receiver?;
        let lambda_slot = match self.ir.expr(*args.get(lambda_parameter as usize)?) {
            IrExpr::GetValue(slot) => *slot,
            _ => return None,
        };
        let declaration_position = statements.iter().position(|statement| {
            matches!(
                self.ir.expr(*statement),
                IrExpr::Variable { index, .. } if *index == lambda_slot
            )
        })?;
        let lambda = match self.ir.expr(statements[declaration_position]).clone() {
            IrExpr::Variable {
                init: Some(lambda), ..
            } => lambda,
            _ => return None,
        };
        let (implementation, captures, inline_body, arity) = match self.ir.expr(lambda).clone() {
            IrExpr::Lambda {
                impl_fn,
                captures,
                inline_body: Some(inline_body),
                arity,
                ..
            } => (impl_fn, captures, inline_body, arity as usize),
            _ => return None,
        };
        if arity != 1 || !self.operand_suspends(inline_body) {
            return None;
        }

        statements.remove(declaration_position);
        let mut capture_declarations = Vec::with_capacity(captures.len());
        let mut formal_slots = Vec::with_capacity(captures.len() + 1);
        for (capture_ordinal, capture) in captures.into_iter().enumerate() {
            if self.ir.shared_capture_parameters.contains_key(&(
                implementation,
                u32::try_from(capture_ordinal).expect("too many inline captures"),
            )) {
                let IrExpr::GetValue(slot) = self.ir.expr(capture) else {
                    return None;
                };
                formal_slots.push(*slot);
                continue;
            }
            let slot = self.allocate_temporary();
            let ty = *self
                .ir
                .functions
                .get(implementation as usize)?
                .params
                .get(capture_ordinal)?;
            capture_declarations.push(self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty,
                init: Some(capture),
                named: false,
            }));
            formal_slots.push(slot);
        }
        statements.splice(
            declaration_position..declaration_position,
            capture_declarations,
        );

        let element_ty = *self
            .ir
            .functions
            .get(implementation as usize)?
            .params
            .get(formal_slots.len())?;
        let part_ty = self.ir.functions.get(implementation as usize)?.ret;
        let element_slot = self.allocate_temporary();
        formal_slots.push(element_slot);
        let local_base = self.next_temporary;
        let local_count =
            rehome_inline_body_values(self.ir, inline_body, &formal_slots, local_base)?;
        self.next_temporary = local_base.checked_add(local_count)?;
        self.ir.functions[implementation as usize].body = None;
        self.ir.inline_only_fns.insert(implementation);

        let factory_call = self.ir.add_expr(IrExpr::New {
            internal: factory_classifier,
            args: Vec::new(),
            ctor_params: Some(Vec::new()),
            ctor_desc: None,
            external_target: Some(factory),
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        let accumulator_slot = self.allocate_temporary();
        statements.push(self.ir.add_expr(IrExpr::Variable {
            index: accumulator_slot,
            ty: accumulator_ty.get(),
            init: Some(factory_call),
            named: true,
        }));

        let iterator_value = self.iterator_call(iterator, iterable).ok()?;
        let iterator_slot = self.allocate_temporary();
        statements.push(self.ir.add_expr(IrExpr::Variable {
            index: iterator_slot,
            ty: iterator_ty.get(),
            init: Some(iterator_value),
            named: true,
        }));
        let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
        let condition = self.iterator_call(has_next, iterator_read).ok()?;
        let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
        let element = self.iterator_call(next, iterator_read).ok()?;
        let element_declaration = self.ir.add_expr(IrExpr::Variable {
            index: element_slot,
            ty: element_ty,
            init: Some(element),
            named: true,
        });

        let (mut body_statements, body_value) = match self.ir.expr(inline_body).clone() {
            IrExpr::Block {
                stmts,
                value: Some(value),
            } => (stmts, value),
            IrExpr::Block { value: None, .. } => return None,
            _ => (Vec::new(), inline_body),
        };
        let part_slot = self.allocate_temporary();
        body_statements.push(self.ir.add_expr(IrExpr::Variable {
            index: part_slot,
            ty: part_ty,
            init: Some(body_value),
            named: false,
        }));
        let part = self.ir.add_expr(IrExpr::GetValue(part_slot));
        let append_argument = if flatten {
            part
        } else {
            self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: part,
                type_operand: append_parameter.get(),
            })
        };
        let accumulator = self.ir.add_expr(IrExpr::GetValue(accumulator_slot));
        let append_call = self.ir.add_expr(IrExpr::Call {
            callee: Callee::External {
                target: append,
                default_provider: None,
                params: vec![append_parameter.get()],
                ret: append_result.get(),
                substitutions: Vec::new(),
                defaults: Vec::new(),
                extension_receiver_parameter: None,
            },
            dispatch_receiver: Some(accumulator),
            args: vec![append_argument],
        });
        self.ir
            .ext_call_source_receiver
            .insert(append_call, accumulator_ty.get());
        body_statements.push(append_call);
        let mut loop_statements = Vec::with_capacity(body_statements.len() + 1);
        loop_statements.push(element_declaration);
        loop_statements.extend(body_statements);
        let loop_body = self.ir.add_expr(IrExpr::Block {
            stmts: loop_statements,
            value: None,
        });
        let loop_label = format!("$fir_inline_collect_{iterator_slot}");
        statements.push(self.ir.add_expr(IrExpr::While {
            cond: condition,
            body: loop_body,
            update: None,
            post_test: false,
            label: Some(loop_label),
        }));
        let result = self.ir.add_expr(IrExpr::GetValue(accumulator_slot));
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: Some(result),
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn external_inline_for_each(
        &mut self,
        lambda_parameter: u32,
        iterator_ty: ResolvedTy,
        iterator: &crate::fir::FirIteratorCall,
        has_next: &crate::fir::FirIteratorCall,
        next: &crate::fir::FirIteratorCall,
        receiver_ty: Option<ResolvedTy>,
        parameter_types: &[Ty],
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
    ) -> Option<ExprId> {
        let (mut statements, receiver, args, defaults) =
            self.selected_semantic_operands(SelectedOperandRequest {
                receiver_ty,
                parameter_types,
                dispatch_receiver,
                extension_receiver,
                arguments,
                defaults: SelectedDefaultMode::Reject,
                preserve_inline_lambdas: false,
                extension_receiver_parameter: None,
                mode: SelectedOperandMode::Materialized,
            })?;
        debug_assert!(defaults.is_empty());
        let iterable = receiver?;
        let lambda_slot = match self.ir.expr(*args.get(lambda_parameter as usize)?) {
            IrExpr::GetValue(slot) => *slot,
            _ => return None,
        };
        let declaration_position = statements.iter().position(|statement| {
            matches!(
                self.ir.expr(*statement),
                IrExpr::Variable { index, .. } if *index == lambda_slot
            )
        })?;
        let lambda = match self.ir.expr(statements[declaration_position]).clone() {
            IrExpr::Variable {
                init: Some(lambda), ..
            } => lambda,
            _ => return None,
        };
        let (implementation, captures, inline_body, arity) = match self.ir.expr(lambda).clone() {
            IrExpr::Lambda {
                impl_fn,
                captures,
                inline_body: Some(inline_body),
                arity,
                ..
            } => (impl_fn, captures, inline_body, arity as usize),
            _ => return None,
        };
        if arity != 1 {
            return None;
        }

        statements.remove(declaration_position);
        let mut capture_declarations = Vec::with_capacity(captures.len());
        let mut formal_slots = Vec::with_capacity(captures.len() + 1);
        for (capture_ordinal, capture) in captures.into_iter().enumerate() {
            if self.ir.shared_capture_parameters.contains_key(&(
                implementation,
                u32::try_from(capture_ordinal).expect("too many inline captures"),
            )) {
                let IrExpr::GetValue(slot) = self.ir.expr(capture) else {
                    return None;
                };
                formal_slots.push(*slot);
                continue;
            }
            let slot = self.allocate_temporary();
            let ty = *self
                .ir
                .functions
                .get(implementation as usize)?
                .params
                .get(capture_ordinal)?;
            capture_declarations.push(self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty,
                init: Some(capture),
                named: false,
            }));
            formal_slots.push(slot);
        }
        statements.splice(
            declaration_position..declaration_position,
            capture_declarations,
        );
        let element_type = *self
            .ir
            .functions
            .get(implementation as usize)?
            .params
            .get(formal_slots.len())?;
        let element_slot = self.allocate_temporary();
        formal_slots.push(element_slot);
        let local_base = self.next_temporary;
        let local_count =
            rehome_inline_body_values(self.ir, inline_body, &formal_slots, local_base)?;
        self.next_temporary = local_base.checked_add(local_count)?;
        self.ir.functions[implementation as usize].body = None;
        self.ir.inline_only_fns.insert(implementation);

        let iterator_value = self.iterator_call(iterator, iterable).ok()?;
        let iterator_slot = self.allocate_temporary();
        let loop_label = format!("$fir_inline_foreach_{iterator_slot}");
        statements.push(self.ir.add_expr(IrExpr::Variable {
            index: iterator_slot,
            ty: iterator_ty.get(),
            init: Some(iterator_value),
            named: false,
        }));
        let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
        let condition = self.iterator_call(has_next, iterator_read).ok()?;
        let iterator_read = self.ir.add_expr(IrExpr::GetValue(iterator_slot));
        let element = self.iterator_call(next, iterator_read).ok()?;
        let element_declaration = self.ir.add_expr(IrExpr::Variable {
            index: element_slot,
            ty: element_type,
            init: Some(element),
            named: true,
        });
        let loop_body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![element_declaration, inline_body],
            value: None,
        });
        let loop_expression = self.ir.add_expr(IrExpr::While {
            cond: condition,
            body: loop_body,
            update: None,
            post_test: false,
            label: Some(loop_label),
        });
        statements.push(loop_expression);
        let unit = self.ir.add_expr(IrExpr::UnitInstance);
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value: Some(unit),
        }))
    }

    pub(super) fn intrinsic_call(
        &mut self,
        operation: crate::ir::IrIntrinsic,
        receiver_ty: Option<ResolvedTy>,
        parameters: &[ResolvedTy],
        result: ResolvedTy,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
    ) -> Option<ExprId> {
        let parameter_types = parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        self.selected_semantic_call(
            Callee::Intrinsic {
                operation,
                ret: result.get(),
            },
            receiver_ty,
            parameter_types,
            dispatch_receiver,
            extension_receiver,
            arguments,
        )
    }

    fn selected_semantic_call(
        &mut self,
        callee: Callee,
        receiver_ty: Option<ResolvedTy>,
        parameter_types: Vec<Ty>,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
    ) -> Option<ExprId> {
        let (statements, receiver, args, defaults) =
            self.selected_semantic_operands(SelectedOperandRequest {
                receiver_ty,
                parameter_types: &parameter_types,
                dispatch_receiver,
                extension_receiver,
                arguments,
                defaults: SelectedDefaultMode::Reject,
                preserve_inline_lambdas: false,
                extension_receiver_parameter: None,
                mode: SelectedOperandMode::DirectWhenOrdered,
            })?;
        debug_assert!(defaults.is_empty());
        let call = self.ir.add_expr(IrExpr::Call {
            callee,
            dispatch_receiver: receiver,
            args,
        });
        Some(self.wrap_call_statements(statements, call))
    }

    pub(super) fn selected_semantic_operands(
        &mut self,
        request: SelectedOperandRequest<'_>,
    ) -> Option<(Vec<ExprId>, Option<ExprId>, Vec<ExprId>, Vec<u32>)> {
        let SelectedOperandRequest {
            receiver_ty,
            parameter_types,
            dispatch_receiver,
            extension_receiver,
            arguments,
            defaults,
            preserve_inline_lambdas,
            extension_receiver_parameter,
            mode,
        } = request;
        let member_extension = dispatch_receiver.is_some() && extension_receiver.is_some();
        if member_extension != extension_receiver_parameter.is_some() {
            return None;
        }
        // A dependency inline call is still expanded by the target from retained bytecode. Keep its
        // non-lambda operands in source-order locals: captured lambda operands then reference those
        // locals, which is the explicit contract consumed by the JVM splicer. Inline lambda literals
        // remain substitution nodes through `preserve_inline_lambdas` below. Ordinary
        // calls have no such retained-body boundary and may keep an already ordered operand stream
        // direct.
        let direct = matches!(mode, SelectedOperandMode::DirectWhenOrdered)
            && !preserve_inline_lambdas
            && !self.checked_operands_suspend(dispatch_receiver, extension_receiver, arguments)
            && arguments_follow_parameter_order(arguments, extension_receiver_parameter);
        let mut statements = Vec::new();
        let receiver = if member_extension {
            dispatch_receiver
        } else {
            dispatch_receiver.or(extension_receiver)
        };
        let receiver = match (receiver, receiver_ty) {
            (Some(receiver), Some(receiver_ty)) if direct => {
                Some(self.direct_call_operand(receiver, receiver_ty.get()))
            }
            (Some(receiver), Some(receiver_ty)) => {
                Some(self.spill_call_operand(receiver, receiver_ty.get(), &mut statements))
            }
            (None, None) => None,
            (Some(_), None) | (None, Some(_)) => return None,
        };
        let extension_operand = if let Some(parameter) = extension_receiver_parameter {
            let parameter = parameter as usize;
            let parameter_ty = *parameter_types.get(parameter)?;
            let extension_receiver = extension_receiver?;
            Some((
                parameter,
                if direct {
                    self.direct_call_operand(extension_receiver, parameter_ty)
                } else {
                    self.spill_call_operand(extension_receiver, parameter_ty, &mut statements)
                },
            ))
        } else {
            None
        };
        let normalized = self.normalize_checked_arguments(
            parameter_types,
            arguments,
            CheckedArgumentPolicy::Selected {
                defaults,
                preserve_inline_lambdas,
            },
            direct,
        )?;
        statements.extend(normalized.statements);
        let mut slots = normalized.slots;
        if let Some((parameter, extension_receiver)) = extension_operand {
            if slots.get(parameter)?.is_some() {
                return None;
            }
            slots[parameter] = Some(extension_receiver);
        }
        let args = if defaults == SelectedDefaultMode::Omit {
            if slots.iter().enumerate().any(|(parameter, slot)| {
                slot.is_none() && !normalized.defaults.contains(&(parameter as u32))
            }) {
                return None;
            }
            slots.into_iter().flatten().collect()
        } else {
            slots.into_iter().collect::<Option<Vec<_>>>()?
        };
        Some((statements, receiver, args, normalized.defaults))
    }

    pub(super) fn wrap_call_statements(&mut self, statements: Vec<ExprId>, call: ExprId) -> ExprId {
        if statements.is_empty() {
            call
        } else {
            self.ir.add_expr(IrExpr::Block {
                stmts: statements,
                value: Some(call),
            })
        }
    }

    pub(super) fn external_constructor_call(
        &mut self,
        declaration: ExternalCallableId,
        classifier: crate::types::TypeName,
        parameters: &[ResolvedTy],
        context_parameter_count: u32,
        outer_parameter: Option<ResolvedTy>,
        outer_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
        annotation: Option<&FirAnnotationConstruction>,
    ) -> Option<ExprId> {
        let parameter_types = parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        let (statements, receiver, mut args, mut defaults) =
            self.selected_semantic_operands(SelectedOperandRequest {
                receiver_ty: None,
                parameter_types: &parameter_types,
                dispatch_receiver: None,
                extension_receiver: None,
                arguments,
                defaults: SelectedDefaultMode::Omit,
                preserve_inline_lambdas: false,
                extension_receiver_parameter: None,
                mode: SelectedOperandMode::DirectWhenOrdered,
            })?;
        debug_assert!(receiver.is_none());
        if context_parameter_count as usize > parameter_types.len() {
            return None;
        }
        for parameter in &mut defaults {
            *parameter = parameter.checked_sub(context_parameter_count)?;
        }
        // An `inner` constructor's enclosing instance is an explicit checked FIR receiver, but is
        // not one of its source value parameters. Put it before those arguments, matching the JVM
        // constructor realization already interned by the provider. Default-mask ordinals remain
        // source-parameter ordinals, so they deliberately are not shifted here.
        if outer_receiver.is_some() != outer_parameter.is_some() {
            return None;
        }
        let mut physical_parameter_types = parameter_types;
        let mut default_prefix_count = context_parameter_count;
        if let Some((outer_receiver, outer_parameter)) = outer_receiver.zip(outer_parameter) {
            args.insert(0, outer_receiver);
            physical_parameter_types.insert(0, outer_parameter.get());
            default_prefix_count = default_prefix_count.checked_add(1)?;
        }
        let construction = self.ir.add_expr(IrExpr::New {
            internal: classifier,
            args,
            ctor_params: Some(physical_parameter_types),
            ctor_desc: None,
            external_target: Some(declaration),
            defaults: defaults.into_boxed_slice(),
            default_prefix_count,
        });
        self.record_external_annotation_construction(construction, classifier, annotation)?;
        Some(self.wrap_call_statements(statements, construction))
    }

    fn record_external_annotation_construction(
        &mut self,
        construction: ExprId,
        classifier: crate::types::TypeName,
        annotation: Option<&FirAnnotationConstruction>,
    ) -> Option<()> {
        let Some(annotation) = annotation else {
            return Some(());
        };
        if annotation.members.len() != annotation.defaults.len() {
            return None;
        }
        let members = annotation
            .members
            .iter()
            .map(|(name, ty)| (name.to_string(), ty.get()))
            .collect::<Vec<_>>();
        let defaults = annotation
            .defaults
            .iter()
            .map(|default| match default {
                Some(default) => self.lower_annotation_default(default).map(Some),
                None => Some(None),
            })
            .collect::<Option<Vec<_>>>()?;
        let enclosing_class = self
            .body
            .lexical_class_owner()
            .and_then(|owner| self.index.classifier_header(owner))
            .map(|owner| owner.classifier);
        self.ir.annotation_constructions.insert(
            construction,
            crate::ir::IrAnnotationConstruction {
                interface: classifier,
                members,
                defaults,
                enclosing_class,
            },
        );
        Some(())
    }

    fn lower_annotation_default(&mut self, default: &FirAnnotationDefaultValue) -> Option<ExprId> {
        let expression = match default {
            FirAnnotationDefaultValue::Singleton(classifier) => IrExpr::SingletonValue {
                classifier: *classifier,
            },
            FirAnnotationDefaultValue::Constant(constant) => IrExpr::Const(match constant {
                FirConstant::Int(value) => IrConst::Int(i32::try_from(*value).ok()?),
                FirConstant::Long(value) | FirConstant::ULong(value) => IrConst::Long(*value),
                FirConstant::UInt(value) => IrConst::Int(u32::try_from(*value).ok()? as i32),
                FirConstant::Double(value) => IrConst::Double(*value),
                FirConstant::Float(value) => IrConst::Float(*value),
                FirConstant::Boolean(value) => IrConst::Boolean(*value),
                FirConstant::String(value) => IrConst::String(value.clone()),
                FirConstant::Char(value) => IrConst::Char(*value),
                FirConstant::Null => IrConst::Null,
            }),
        };
        Some(self.ir.add_expr(expression))
    }

    pub(super) fn module_constructor_call(
        &mut self,
        request: ModuleConstructorRequest<'_>,
    ) -> Option<ExprId> {
        let ModuleConstructorRequest {
            target,
            classifier,
            argument_parameter_types,
            declaration_parameter_types,
            primary_in_current_file,
            context_parameter_count,
            outer_receiver,
            external_capture_arguments,
            arguments,
        } = request;
        let mut declaration_parameter_types = declaration_parameter_types.to_vec();
        let selected = self.selected_semantic_operands(SelectedOperandRequest {
            receiver_ty: None,
            parameter_types: argument_parameter_types,
            dispatch_receiver: None,
            extension_receiver: None,
            arguments,
            defaults: SelectedDefaultMode::Omit,
            preserve_inline_lambdas: false,
            extension_receiver_parameter: None,
            mode: SelectedOperandMode::DirectWhenOrdered,
        });
        crate::trace_compiler!(
            "lower",
            "module constructor classifier={classifier} parameters={argument_parameter_types:?} arguments={arguments:?} selected={}",
            selected.is_some(),
        );
        let (statements, receiver, mut args, mut defaults) = selected?;
        debug_assert!(receiver.is_none());
        if context_parameter_count as usize > argument_parameter_types.len() {
            return None;
        }
        for parameter in &mut defaults {
            *parameter = parameter.checked_sub(context_parameter_count)?;
        }
        let mut default_prefix_count = context_parameter_count;
        if let Some(outer_receiver) = outer_receiver {
            let declaration = self.index.classifier_declaration(classifier)?;
            let outer = self.index.enclosing_owner_classifier(declaration)?;
            args.insert(0, outer_receiver);
            let outer = Ty::obj_name(outer.classifier);
            declaration_parameter_types.insert(0, outer);
            default_prefix_count += 1;
        }
        if let Some(captures) = external_capture_arguments {
            let capture_count = captures.len() as u32;
            args.splice(0..0, captures.iter().map(|(value, _)| *value));
            declaration_parameter_types.splice(0..0, captures.iter().map(|(_, ty)| *ty));
            default_prefix_count += capture_count;
        } else if let Some(captures) = self.local_class_captures.get(&classifier) {
            let capture_count = captures.len() as u32;
            args.splice(0..0, captures.iter().map(|(value, _)| *value));
            declaration_parameter_types.splice(0..0, captures.iter().map(|(_, ty)| *ty));
            default_prefix_count += capture_count;
        }
        let construction = self.ir.add_expr(IrExpr::New {
            internal: classifier,
            args,
            ctor_params: (!primary_in_current_file).then_some(declaration_parameter_types),
            ctor_desc: None,
            external_target: None,
            defaults: defaults.into_boxed_slice(),
            default_prefix_count,
        });
        self.record_module_annotation_construction(construction, target, classifier)?;
        Some(self.wrap_call_statements(statements, construction))
    }

    fn record_module_annotation_construction(
        &mut self,
        construction: ExprId,
        target: CallableId,
        classifier: crate::types::TypeName,
    ) -> Option<()> {
        let declaration = self.index.classifier_declaration(classifier)?;
        let header = self.index.declaration_header(declaration)?;
        if !header
            .flags
            .has(crate::fir::DeclarationFlags::ANNOTATION_CLASS)
        {
            return Some(());
        }
        let callable = self.index.callable(target)?;
        let signature = self.index.signature(callable.declaration)?;
        let members = signature
            .parameters
            .iter()
            .enumerate()
            .map(|(ordinal, parameter)| {
                Some((
                    self.index
                        .callable_parameter_name(target, ordinal as u32)?
                        .to_owned(),
                    crate::types::stored_value_ty(parameter.get()),
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        let defaults = self
            .ir
            .class_ctor_defaults_name(classifier)
            .cloned()
            .unwrap_or_else(|| vec![None; members.len()]);
        let enclosing_class = self
            .body
            .lexical_class_owner()
            .and_then(|owner| self.index.classifier_header(owner))
            .map(|owner| owner.classifier);
        self.ir.annotation_constructions.insert(
            construction,
            crate::ir::IrAnnotationConstruction {
                interface: classifier,
                members,
                defaults,
                enclosing_class,
            },
        );
        Some(())
    }

    /// Normalize source-order checked arguments into physical parameter order. An already ordered
    /// operand stream stays direct; a reordered stream is first moved to body-local temporaries so
    /// named arguments cannot reorder user effects. Default and vararg decisions are consumed here
    /// rather than reconstructed by the backend.
    pub(super) fn same_file_call(
        &mut self,
        target: CallableId,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[IrCheckedArgument],
        specialized_parameters: &[Ty],
        substitutions: &[crate::fir::FirTypeSubstitution],
    ) -> Option<ExprId> {
        let callable = self.index.callable(target)?;
        let declaration = self.index.declaration_anchor(callable.declaration)?;
        if declaration.kind != DeclarationKind::Function {
            return None;
        }
        let signature = self.index.signature(callable.declaration)?;
        let declaration_flags = self.index.declaration_header(callable.declaration)?.flags;
        let companion_associated = declaration_flags.has(crate::fir::DeclarationFlags::COMPANION);
        let suspend = declaration_flags.has(crate::fir::DeclarationFlags::SUSPEND);
        if companion_associated && extension_receiver.is_some() {
            return None;
        }
        let declared_extension_receiver = (!companion_associated)
            .then_some(callable.shape.extension_receiver)
            .flatten();
        let function = self.ir.checked_callable_functions.get(&target).copied();
        let enum_entry_class = declaration
            .owner
            .and_then(|owner| self.ir.checked_enum_entry_classes.get(&owner).copied());
        let mut statements = Vec::new();
        // An extension receiver is inserted among context/value parameters below. Keep that rarer
        // shape on the general spill path; an ordinary receiver plus already ordered value arguments
        // maps directly to the JVM operand order without any temporary.
        let direct = extension_receiver.is_none()
            && !declaration_flags.has(crate::fir::DeclarationFlags::TAILREC)
            && !self.checked_operands_suspend(dispatch_receiver, extension_receiver, arguments)
            && arguments_follow_parameter_order(arguments, None);
        let bindings = substitutions
            .iter()
            .filter_map(|substitution| match substitution.parameter {
                crate::fir::FirTypeParameterRef::Module(parameter) => self
                    .index
                    .type_parameter_semantic_name(parameter)
                    .map(|name| (name.to_owned(), substitution.value.get())),
                crate::fir::FirTypeParameterRef::External { .. } => None,
            })
            .collect::<std::collections::HashMap<_, _>>();

        let dispatch_receiver = match dispatch_receiver {
            Some(receiver) if direct => {
                let classifier = self.index.enclosing_classifier(callable.declaration)?;
                Some(self.direct_call_operand(receiver, Ty::obj_name(classifier.classifier)))
            }
            Some(receiver) => {
                let classifier = self.index.enclosing_classifier(callable.declaration)?;
                let semantic_receiver = Ty::obj_name(classifier.classifier);
                let storage_receiver = self
                    .ir
                    .physical_types
                    .get(&receiver)
                    .copied()
                    .unwrap_or(semantic_receiver);
                Some(self.spill_call_operand(receiver, storage_receiver, &mut statements))
            }
            None => None,
        };
        let extension_receiver = match extension_receiver {
            Some(receiver) => {
                let ty = declared_extension_receiver?;
                let specialized = crate::types::ty_subst_keep_unbound(ty.get(), &bindings);
                let receiver = self.spill_call_operand(receiver, specialized, &mut statements);
                Some(if !specialized.is_reference() && ty.get().is_reference() {
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg: receiver,
                        type_operand: ty.get(),
                    })
                } else {
                    receiver
                })
            }
            None => None,
        };

        let declared_parameters = signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        let normalized = self.normalize_checked_arguments(
            specialized_parameters,
            arguments,
            CheckedArgumentPolicy::SameFileInline {
                declared_parameters: &declared_parameters,
                inline: callable.is_inline(),
            },
            direct,
        )?;
        statements.extend(normalized.statements);
        let mut slots = normalized.slots;
        let mut inline_lambdas = normalized.inline_lambdas;

        let extension_position = callable.shape.context_parameter_count as usize;
        if let Some(receiver) = extension_receiver {
            if extension_position > slots.len() {
                return None;
            }
            slots.insert(extension_position, Some(receiver));
            inline_lambdas.insert(extension_position, None);
        }
        let default_argument_positions = slots
            .iter()
            .enumerate()
            .filter_map(|(position, argument)| {
                argument
                    .is_none()
                    .then(|| u32::try_from(position).ok())
                    .flatten()
            })
            .collect::<Vec<_>>();
        let has_defaults = slots.iter().any(Option::is_none);
        let mut parameter_types = declared_parameters;
        if let Some(receiver) = declared_extension_receiver {
            parameter_types.insert(extension_position, receiver.get());
        }
        // Keep the declaration's unspecialized signature separate from the selected semantic
        // argument/result types. A sibling generic member `fun <T> id(T): T`, selected as
        // `id<String>`, still links as `(Object)Object`; the expression-level coercion below turns
        // that physical result into `String`.
        let mut declaration_parameter_types = signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        if let Some(receiver) = declared_extension_receiver {
            declaration_parameter_types.insert(extension_position, receiver.get());
        }
        let selected_declaration_parameter_types = declaration_parameter_types.clone();
        let declaration_result = signature.result.get();
        // A member called from the same lexical classifier needs no target access bridge: cloning its
        // retained checked template preserves the exact `this` operand and lets literal lambda
        // arguments splice in common IR. Value-class, singleton, and companion members are physically
        // reshaped by target backends, so they also consume the checked template while the semantic
        // source receiver is still explicit. An arbitrary cross-class member still needs the
        // declaration-owned private-access and generic-receiver adaptation path.
        let enclosing_classifier = self.index.enclosing_classifier(callable.declaration);
        let common_member_inline = dispatch_receiver.is_none()
            || enclosing_classifier.is_some_and(|classifier| {
                self.body.lexical_class_owner() == Some(classifier.declaration)
                    || self
                        .index
                        .declaration_header(classifier.declaration)
                        .is_some_and(|header| {
                            header.flags.has(crate::fir::DeclarationFlags::VALUE)
                                || header.flags.has(crate::fir::DeclarationFlags::SINGLETON)
                                || header.flags.has(crate::fir::DeclarationFlags::COMPANION)
                        })
            });
        // A lambda containing a checked return through an enclosing callable boundary has no
        // independently executable JVM shape: its apparent lambda result can differ from the
        // enclosing function's return value. It must be spliced while that lexical return target is
        // still present, even when the inline member belongs to another ordinary class.
        let has_nonlocal_inline_return = inline_lambdas.iter().flatten().any(|lambda| {
            let IrExpr::Lambda {
                inline_body: Some(body),
                ..
            } = self.ir.expr(*lambda)
            else {
                return false;
            };
            !super::inline_returns::reachable_checked_returns(self.ir, *body).is_empty()
        });
        crate::trace_compiler!(
            "lower",
            "same-file call target={target:?} inline={} function={function:?} common_member_inline={common_member_inline} defaults={has_defaults} nonlocal={has_nonlocal_inline_return} substitutions={substitutions:?}"
            , callable.is_inline()
        );
        if callable.is_inline()
            && (common_member_inline || has_nonlocal_inline_return)
            && !has_defaults
        {
            if let Some(function) = function {
                let mut operands =
                    Vec::with_capacity(slots.len() + usize::from(dispatch_receiver.is_some()));
                let mut inlined_lambda_operands = Vec::with_capacity(
                    inline_lambdas.len() + usize::from(dispatch_receiver.is_some()),
                );
                if let Some(receiver) = dispatch_receiver {
                    operands.push(receiver);
                    inlined_lambda_operands.push(None);
                }
                operands.extend(slots.iter().copied().collect::<Option<Vec<_>>>()?);
                inlined_lambda_operands.extend(inline_lambdas.iter().copied());
                if let Some(inlined) = self.inline_same_file_call(
                    target,
                    function,
                    &operands,
                    &inlined_lambda_operands,
                    substitutions,
                ) {
                    let expanded = if statements.is_empty() {
                        inlined
                    } else {
                        self.ir.add_expr(IrExpr::Block {
                            stmts: statements,
                            value: Some(inlined),
                        })
                    };
                    self.ir.inline_regions.insert(expanded);
                    return Some(expanded);
                }
            }
        }
        let physical_function =
            function.filter(|function| !self.ir.foreign_inline_templates.contains(function));
        let call = match dispatch_receiver {
            Some(receiver) if physical_function.is_some() => {
                let function = physical_function.expect("same-file callable realization");
                let class = match enum_entry_class {
                    Some(class) => class,
                    None => {
                        let classifier = self.index.enclosing_classifier(callable.declaration)?;
                        self.ir
                            .checked_classifier_classes
                            .get(&classifier.declaration)
                            .copied()?
                    }
                };
                let method = self.ir.classes[class as usize]
                    .methods
                    .iter()
                    .position(|candidate| *candidate == function)?
                    as u32;
                let receiver = if enum_entry_class.is_some() {
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: crate::ir::IrTypeOp::Cast,
                        arg: receiver,
                        type_operand: Ty::obj_name(self.ir.classes[class as usize].fq_name_id()),
                    })
                } else {
                    receiver
                };
                self.ir.add_expr(IrExpr::MethodCall {
                    class,
                    index: method,
                    receiver,
                    args: slots,
                })
            }
            Some(receiver) if !has_defaults => {
                let classifier = self.index.enclosing_classifier(callable.declaration)?;
                let flags = self.index.declaration_header(classifier.declaration)?.flags;
                self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Virtual {
                        owner: classifier.classifier,
                        name: self.index.callable_name(target)?.to_owned(),
                        descriptor: String::new(),
                        params: Some((declaration_parameter_types, declaration_result)),
                        interface: flags.has(crate::fir::DeclarationFlags::INTERFACE),
                    },
                    dispatch_receiver: Some(receiver),
                    args: slots.into_iter().collect::<Option<Vec<_>>>()?,
                })
            }
            Some(receiver) if physical_function.is_none() => {
                // Preserve the checked source call. A target backend owns the static bridge, masks,
                // marker parameter, and dispatch-receiver placement.
                self.ir.add_expr(IrExpr::Call {
                    callee: Callee::ModuleWithDefaults {
                        target,
                        name: self.index.callable_name(target)?.to_owned(),
                        params: declaration_parameter_types,
                        ret: declaration_result,
                        defaults: default_argument_positions.clone().into_boxed_slice(),
                        dispatch_receiver_ty: enclosing_classifier
                            .map(|classifier| Ty::obj_name(classifier.classifier)),
                        extension_receiver_parameter: declared_extension_receiver.map(|_| {
                            u32::try_from(extension_position).expect("too many source parameters")
                        }),
                    },
                    dispatch_receiver: Some(receiver),
                    args: slots.into_iter().flatten().collect(),
                })
            }
            Some(_) => return None,
            None if has_defaults => {
                if physical_function.is_none() {
                    self.ir.add_expr(IrExpr::Call {
                        callee: Callee::ModuleWithDefaults {
                            target,
                            name: self.index.callable_name(target)?.to_owned(),
                            params: declaration_parameter_types,
                            ret: declaration_result,
                            defaults: default_argument_positions.clone().into_boxed_slice(),
                            dispatch_receiver_ty: None,
                            extension_receiver_parameter: declared_extension_receiver.map(|_| {
                                u32::try_from(extension_position)
                                    .expect("too many source parameters")
                            }),
                        },
                        dispatch_receiver: None,
                        args: slots.into_iter().flatten().collect(),
                    })
                } else {
                    let function =
                        physical_function.expect("same-file default callable realization");
                    self.ir.add_expr(IrExpr::Call {
                        callee: Callee::LocalWithDefaults {
                            function,
                            defaults: default_argument_positions.clone().into_boxed_slice(),
                        },
                        dispatch_receiver: None,
                        args: slots.into_iter().flatten().collect(),
                    })
                }
            }
            None if !has_defaults && physical_function.is_some() => {
                self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Local(
                        physical_function.expect("same-file callable realization"),
                    ),
                    dispatch_receiver: None,
                    args: slots.into_iter().collect::<Option<Vec<_>>>()?,
                })
            }
            None if !has_defaults => self.ir.add_expr(IrExpr::Call {
                callee: Callee::Module {
                    target,
                    name: self.index.callable_name(target)?.to_owned(),
                    params: declaration_parameter_types,
                    ret: declaration_result,
                },
                dispatch_receiver: None,
                args: slots.into_iter().collect::<Option<Vec<_>>>()?,
            }),
            None => return None,
        };
        // Preserve the declaration's unspecialized result on the concrete call node. Value-class
        // realization needs this checked distinction: a member declared to return `X` yields X's raw
        // carrier, while a generic `T` merely specialized to `X` yields a boxed value across erasure.
        // External/library calls publish the same fact in their respective constructors.
        self.ir.call_declared_ret.insert(call, declaration_result);
        if !has_defaults {
            self.ir.call_declared_params.insert(
                call,
                selected_declaration_parameter_types.into_boxed_slice(),
            );
        }
        // A sibling-source callable is realized into `CrossFile` only after common lowering. Keep
        // its checked suspend behavior on the concrete expression now, while the stable module
        // declaration is still available; JVM coroutine lowering must append the continuation and
        // must not infer suspendness from the eventual method name or descriptor.
        if suspend {
            self.ir.suspend_calls.insert(call, signature.result.get());
        }
        Some(if statements.is_empty() {
            call
        } else {
            self.ir.add_expr(IrExpr::Block {
                stmts: statements,
                value: Some(call),
            })
        })
    }

    /// Consume the checker's parameter mapping once for all ordinary source calls. This is the
    /// only lowering operation that groups repeated vararg fragments, preserves source evaluation
    /// order, and distinguishes an omitted default from an expression. Call kinds may choose how a
    /// present value crosses their semantic boundary, but must not reimplement argument mapping.
    fn normalize_checked_arguments(
        &mut self,
        parameter_types: &[Ty],
        arguments: &[IrCheckedArgument],
        policy: CheckedArgumentPolicy<'_>,
        direct: bool,
    ) -> Option<NormalizedCheckedArguments> {
        let mut statements = Vec::new();
        let mut inline_lambdas = vec![None; parameter_types.len()];
        let mut defaults = Vec::new();
        let slots = materialize_checked_arguments(
            arguments,
            parameter_types.len(),
            |parameter| Some(parameter as usize),
            |parameter, argument| {
                let parameter_ty = *parameter_types.get(parameter)?;
                match argument {
                    CheckedArgumentValue::Expression(value) => {
                        let preserve = match policy {
                            CheckedArgumentPolicy::Selected {
                                preserve_inline_lambdas,
                                ..
                            } => {
                                preserve_inline_lambdas
                                    && matches!(
                                        self.ir.expr(value),
                                        IrExpr::Lambda {
                                            inline_body: Some(_),
                                            ..
                                        }
                                    )
                            }
                            CheckedArgumentPolicy::SameFileInline { inline, .. } => {
                                inline
                                    && matches!(
                                        self.ir.expr(value),
                                        IrExpr::Lambda {
                                            inline_body: Some(_),
                                            ..
                                        }
                                    )
                            }
                        };
                        if matches!(policy, CheckedArgumentPolicy::SameFileInline { .. })
                            && preserve
                        {
                            inline_lambdas[parameter] = Some(value);
                        }
                        let value = if preserve {
                            value
                        } else if direct {
                            self.direct_call_operand(value, parameter_ty)
                        } else {
                            self.spill_call_operand(value, parameter_ty, &mut statements)
                        };
                        Some(match policy {
                            CheckedArgumentPolicy::SameFileInline {
                                declared_parameters,
                                ..
                            } => {
                                let declared = *declared_parameters.get(parameter)?;
                                if !parameter_ty.is_reference() && declared.is_reference() {
                                    self.ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::ImplicitCoercion,
                                        arg: value,
                                        type_operand: declared,
                                    })
                                } else {
                                    value
                                }
                            }
                            CheckedArgumentPolicy::Selected { .. } => value,
                        })
                    }
                    CheckedArgumentValue::VarargElement {
                        value,
                        array_type,
                        spread,
                    } => {
                        let element_ty = array_type.array_elem()?;
                        Some(if direct {
                            self.direct_call_operand(
                                value,
                                if spread { array_type } else { element_ty },
                            )
                        } else {
                            self.spill_call_operand(
                                value,
                                if spread { array_type } else { element_ty },
                                &mut statements,
                            )
                        })
                    }
                }
            },
        )?;
        let slots = slots
            .into_iter()
            .enumerate()
            .map(|(parameter, slot)| match slot {
                CheckedArgumentSlot::Missing => Some(None),
                CheckedArgumentSlot::Expression(value) => Some(Some(value)),
                CheckedArgumentSlot::Default(ordinal) => match policy {
                    CheckedArgumentPolicy::Selected {
                        defaults:
                            mode @ (SelectedDefaultMode::Omit | SelectedDefaultMode::Materialize),
                        ..
                    } => {
                        defaults.push(ordinal);
                        if mode == SelectedDefaultMode::Omit {
                            Some(None)
                        } else {
                            let parameter_ty = *parameter_types.get(parameter)?;
                            Some(Some(self.ir.add_expr(IrExpr::Const(
                                IrConst::zero_for_value_type(parameter_ty),
                            ))))
                        }
                    }
                    CheckedArgumentPolicy::Selected {
                        defaults: SelectedDefaultMode::Reject,
                        ..
                    } => None,
                    CheckedArgumentPolicy::SameFileInline { .. } => Some(None),
                },
                CheckedArgumentSlot::Vararg {
                    array_type,
                    elements,
                    spreads,
                } => Some(Some(self.ir.add_expr(IrExpr::Vararg {
                    array_type,
                    spreads,
                    elements,
                }))),
            })
            .collect::<Option<Vec<_>>>()?;
        Some(NormalizedCheckedArguments {
            statements,
            slots,
            inline_lambdas,
            defaults,
        })
    }

    pub(super) fn spill_call_operand(
        &mut self,
        value: ExprId,
        ty: Ty,
        statements: &mut Vec<ExprId>,
    ) -> ExprId {
        let temporary = self.allocate_temporary();
        statements.push(self.ir.add_expr(IrExpr::Variable {
            index: temporary,
            ty,
            init: Some(value),
            named: false,
        }));
        let read = self.ir.add_expr(IrExpr::GetValue(temporary));
        // This read is generated while normalizing checked argument evaluation order, so it has no
        // source FIR node of its own. Preserve the already-specialized semantic operand type anyway:
        // a backend may need to distinguish `UInt` from its `int` carrier when the selected
        // declaration stores the value in an erased reference slot.
        self.ir.logical_types.insert(read, ty);
        read
    }

    /// Preserve the checker-selected logical operand type on the allocation-free ordered path.
    /// Any representation-changing conversion is already explicit on the checked FIR receiver or
    /// argument and was lowered before this point. The consumer's selected parameter is already on
    /// the call node; writing it onto `value` would mutate the producer's semantic type (`Char - Int`
    /// consumed by `compareTo(Int)` must remain `Char`).
    fn direct_call_operand(&mut self, value: ExprId, _target: Ty) -> ExprId {
        value
    }
}

/// Move a lambda's independently numbered value-producing body into its enclosing callable.
/// Captures and lambda parameters map to already-evaluated outer slots; lambda-local slots are
/// packed above the enclosing body's current temporary high-water mark. Nested lambda bodies retain
/// their own numbering, while their capture operands are remapped with the surrounding expression.
pub(super) fn rehome_inline_body_values(
    ir: &mut crate::ir::IrFile,
    root: ExprId,
    formal_slots: &[u32],
    local_base: u32,
) -> Option<u32> {
    fn mapped(index: u32, formal_slots: &[u32], local_base: u32) -> Option<(u32, u32)> {
        if let Some(slot) = formal_slots.get(index as usize) {
            return Some((*slot, 0));
        }
        let local = index.checked_sub(formal_slots.len() as u32)?;
        Some((local_base.checked_add(local)?, local.checked_add(1)?))
    }

    fn visit(
        ir: &mut crate::ir::IrFile,
        expression: ExprId,
        formal_slots: &[u32],
        local_base: u32,
        local_count: &mut u32,
        visited: &mut std::collections::HashSet<ExprId>,
    ) -> Option<()> {
        if !visited.insert(expression) {
            return Some(());
        }
        match &mut ir.exprs[expression as usize] {
            IrExpr::GetValue(index) => {
                let (mapped, count) = mapped(*index, formal_slots, local_base)?;
                *index = mapped;
                *local_count = (*local_count).max(count);
            }
            IrExpr::SetValue { var, .. } | IrExpr::Variable { index: var, .. } => {
                let (mapped, count) = mapped(*var, formal_slots, local_base)?;
                *var = mapped;
                *local_count = (*local_count).max(count);
            }
            IrExpr::Try { catches, .. } => {
                for catch in catches {
                    let (mapped, count) = mapped(catch.var, formal_slots, local_base)?;
                    catch.var = mapped;
                    *local_count = (*local_count).max(count);
                }
            }
            _ => {}
        }
        if let IrExpr::Lambda { captures, .. } = &ir.exprs[expression as usize] {
            let captures = captures.clone();
            for capture in captures {
                visit(ir, capture, formal_slots, local_base, local_count, visited)?;
            }
            return Some(());
        }
        let mut children = Vec::new();
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| children.push(child));
        for child in children {
            visit(ir, child, formal_slots, local_base, local_count, visited)?;
        }
        Some(())
    }

    let mut local_count = 0;
    visit(
        ir,
        root,
        formal_slots,
        local_base,
        &mut local_count,
        &mut std::collections::HashSet::new(),
    )?;
    Some(local_count)
}
