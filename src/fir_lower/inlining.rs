//! Call-site expansion of retained same-module inline bodies.
//!
//! Pass 1 retains checked FIR only for semantic inline declarations. The file sink lowers those
//! bodies before ordinary callers; this module clones that checked common-IR template at a call,
//! applies the checker's type substitutions, rebases body-local values, and turns inline-function
//! returns into an expression-local loop exit.

use std::collections::{HashMap, HashSet};

use crate::fir::{
    CallableId, FirCallableReferenceTarget, FirConstructorTarget, FirPropertyReferenceTarget,
    FirPropertyTarget, FirReferenceAdaptation, FirTypeParameterRef, FirTypeSubstitution,
    ResolvedTy,
};
use crate::ir::{
    Callee, ExprId, IrCheckedArgument, IrCheckedOperation, IrCheckedSubstitution, IrConst, IrExpr,
    IrIntrinsic, IrSamTarget, IrValueClassSuspendResult,
};
use crate::types::{stored_value_ty, ty_subst_keep_unbound, Ty};

use super::BodyLowering;

impl BodyLowering<'_> {
    pub(super) fn inline_same_file_call(
        &mut self,
        target: CallableId,
        function: crate::ir::FunId,
        operands: &[ExprId],
        inline_lambdas: &[Option<ExprId>],
        substitutions: &[FirTypeSubstitution],
    ) -> Option<ExprId> {
        let template = self.ir.functions.get(function as usize)?.body?;
        let function_shape = self.ir.functions.get(function as usize)?;
        let parameter_count = u32::try_from(
            function_shape.params.len() + usize::from(function_shape.dispatch_receiver.is_some()),
        )
        .ok()?;
        if operands.len() != parameter_count as usize || inline_lambdas.len() != operands.len() {
            return None;
        }
        let bindings = substitutions
            .iter()
            .filter_map(|substitution| match substitution.parameter {
                FirTypeParameterRef::Module(parameter) => self
                    .index
                    .type_parameter_semantic_name(parameter)
                    .map(|name| (name.to_owned(), substitution.value.get())),
                FirTypeParameterRef::External { .. } => None,
            })
            .collect::<HashMap<_, _>>();
        let operand_types = function_shape
            .dispatch_receiver
            .map(Ty::obj_name)
            .into_iter()
            .chain(function_shape.params.iter().copied())
            .map(|ty| ty_subst_keep_unbound(ty, &bindings))
            .collect::<Vec<_>>();
        let operands = operands
            .iter()
            .map(|operand| self.specialized_inline_operand(*operand, &bindings))
            .collect::<Vec<_>>();
        let mut operand_declarations = Vec::new();
        let operand_slots = operands
            .iter()
            .zip(inline_lambdas)
            .zip(operand_types)
            .map(
                |((operand, lambda), ty)| match (self.ir.expr(*operand), lambda) {
                    (IrExpr::GetValue(slot), None) => Some(*slot),
                    (IrExpr::Lambda { .. }, Some(_)) => return None,
                    (_, None) => {
                        let slot = self.allocate_temporary();
                        operand_declarations.push(self.ir.add_expr(IrExpr::Variable {
                            index: slot,
                            ty: stored_value_ty(ty),
                            init: Some(*operand),
                            named: false,
                        }));
                        Some(slot)
                    }
                    _ => return None,
                },
            )
            .collect::<Vec<_>>();
        crate::trace_compiler!(
            "lower",
            "inline target={target:?} substitutions={substitutions:?} bindings={bindings:?}"
        );
        let result_ty = ty_subst_keep_unbound(self.ir.functions[function as usize].ret, &bindings);

        // A lambda's `inline_body` has its own value numbering and return target. It is cloned for
        // later HOF splicing, but must not be rewritten as part of the enclosing inline function.
        let mut protected = HashSet::new();
        let mut pending = vec![template];
        let mut seen = HashSet::new();
        while let Some(expression) = pending.pop() {
            if !seen.insert(expression) {
                continue;
            }
            if let IrExpr::Lambda {
                captures,
                inline_body,
                ..
            } = self.ir.expr(expression)
            {
                pending.extend(captures.iter().copied());
                if let Some(body) = inline_body {
                    mark_subtree(self.ir, *body, &mut protected);
                }
                continue;
            }
            crate::ir::for_each_child(&self.ir.exprs, expression, &mut |child| pending.push(child));
        }

        let (cloned_root, cloned) = crate::ir::clone_expression_dag(self.ir, template);
        let highest_local = cloned
            .keys()
            .filter(|source| !protected.contains(source))
            .filter_map(|source| value_indices(self.ir.expr(*source)).into_iter().max())
            .filter(|index| *index >= parameter_count)
            .max();
        let local_count = highest_local
            .map(|highest| highest + 1 - parameter_count)
            .unwrap_or(0);
        let local_base = self.next_temporary;
        self.next_temporary = self.next_temporary.checked_add(local_count)?;

        let result_slot = (result_ty != Ty::Unit).then(|| {
            let slot = self.next_temporary;
            self.next_temporary += 1;
            slot
        });
        let label = format!("$fir_inline${}_{}", target.raw(), self.next_temporary);

        for (&source, &copy) in &cloned {
            let generated_zero = match self.ir.expr(source) {
                IrExpr::Variable {
                    ty,
                    init: Some(initial),
                    named: false,
                    ..
                } => match self.ir.expr(*initial) {
                    IrExpr::Const(value) if *value == IrConst::zero_for_value_type(*ty) => {
                        Some(value.clone())
                    }
                    _ => None,
                },
                _ => None,
            };
            // Reified/type-parameter substitutions are lexical: they apply inside nested lambda
            // templates even though those templates own an independent value-numbering domain.
            // Value rebasing and return rewriting remain protected below, but the checked type
            // decision must cross the enclosing inline-call boundary with the lambda.
            specialize_expression_facts(self.ir, copy, &bindings);
            specialize_types(self.ir.exprs.get_mut(copy as usize)?, &bindings);
            if let Some(previous_zero) = generated_zero {
                let replacement = match self.ir.expr(copy) {
                    IrExpr::Variable { ty, .. } => IrConst::zero_for_value_type(*ty),
                    _ => continue,
                };
                if replacement != previous_zero {
                    let initial = self.ir.add_expr(IrExpr::Const(replacement));
                    if let IrExpr::Variable { init, .. } = self.ir.exprs.get_mut(copy as usize)? {
                        *init = Some(initial);
                    }
                }
            }
            if protected.contains(&source) {
                continue;
            }
            if let IrExpr::GetValue(parameter) = self.ir.expr(source) {
                if let Some(Some(lambda)) = inline_lambdas.get(*parameter as usize) {
                    self.ir.exprs[copy as usize] = self.ir.expr(*lambda).clone();
                    if let Some(ty) = self.ir.logical_types.get(lambda).copied() {
                        self.ir.logical_types.insert(copy, ty);
                    }
                    continue;
                }
            }
            rebase_values(
                self.ir.exprs.get_mut(copy as usize)?,
                parameter_count,
                &operand_slots,
                local_base,
            )?;

            let returned = match self.ir.expr(copy).clone() {
                IrExpr::Return(value) => Some(value),
                _ => None,
            };
            if let Some(value) = returned {
                // This return has crossed its checked callable boundary and is now represented by
                // the expression-local break below.  The sparse depth fact belongs to the old
                // `Return` node shape; leaving it on the replacement block makes an enclosing
                // inline-lambda template try to consume the same return a second time.
                self.ir.checked_return_depths.remove(&copy);
                let exit = self.ir.add_expr(IrExpr::Break {
                    label: Some(label.clone()),
                });
                self.ir.exprs[copy as usize] =
                    if let (Some(slot), Some(value)) = (result_slot, value) {
                        let assign = self.ir.add_expr(IrExpr::SetValue { var: slot, value });
                        IrExpr::Block {
                            stmts: vec![assign, exit],
                            value: None,
                        }
                    } else {
                        IrExpr::Block {
                            stmts: vec![exit],
                            value: None,
                        }
                    };
            }
        }

        let inline_invocations = cloned
            .values()
            .copied()
            .filter(|expression| {
                matches!(
                    self.ir.expr(*expression),
                    IrExpr::InvokeFunction { func, .. }
                        if matches!(
                            self.ir.expr(*func),
                            IrExpr::Lambda { inline_body: Some(_), .. }
                        )
                )
            })
            .collect::<Vec<_>>();
        for invocation in inline_invocations {
            self.splice_inline_lambda_invocation(invocation)?;
        }

        let mut statements = operand_declarations;
        if let Some(slot) = result_slot {
            let initial = self
                .ir
                .add_expr(IrExpr::Const(IrConst::zero_for_value_type(result_ty)));
            statements.push(self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty: stored_value_ty(result_ty),
                init: Some(initial),
                named: false,
            }));
        }
        let fallthrough = self.ir.add_expr(IrExpr::Break {
            label: Some(label.clone()),
        });
        let loop_body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![cloned_root, fallthrough],
            value: None,
        });
        let condition = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        statements.push(self.ir.add_expr(IrExpr::While {
            cond: condition,
            body: loop_body,
            update: None,
            post_test: false,
            label: Some(label),
        }));
        let value = result_slot
            .map(|slot| self.ir.add_expr(IrExpr::GetValue(slot)))
            .or_else(|| Some(self.ir.add_expr(IrExpr::UnitInstance)));
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: statements,
            value,
        }))
    }

    /// Specialize the call-boundary coercion that was retained for the generic declaration's
    /// callable ABI. Once the checked body is spliced, that erased boundary no longer exists: the
    /// operand and cloned parameter use the selected concrete type directly. Clone the wrapper so
    /// another consumer of the original expression cannot observe this call's substitutions.
    fn specialized_inline_operand(
        &mut self,
        operand: ExprId,
        bindings: &HashMap<String, Ty>,
    ) -> ExprId {
        let IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } = self.ir.expr(operand).clone()
        else {
            return operand;
        };
        let specialized = ty_subst_keep_unbound(type_operand, bindings);
        if specialized == type_operand {
            return operand;
        }
        let expression = self.ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            type_operand: specialized,
        });
        self.ir.logical_types.insert(expression, specialized);
        expression
    }

    pub(super) fn splice_inline_lambda_invocation(&mut self, invocation: ExprId) -> Option<()> {
        let IrExpr::InvokeFunction {
            func,
            args,
            params: _,
            ret: _,
        } = self.ir.expr(invocation).clone()
        else {
            return None;
        };
        let IrExpr::Lambda {
            impl_fn,
            arity,
            captures,
            inline_body: Some(inline_body),
            ..
        } = self.ir.expr(func).clone()
        else {
            return None;
        };
        if args.len() != arity as usize {
            return None;
        }
        let parameter_types = self.ir.functions.get(impl_fn as usize)?.params.clone();
        if parameter_types.len() != captures.len() + args.len() {
            return None;
        }

        let mut declarations = Vec::new();
        let mut formal_slots = Vec::with_capacity(parameter_types.len());
        for (value, ty) in captures.into_iter().chain(args).zip(parameter_types) {
            let slot = match self.ir.expr(value) {
                IrExpr::GetValue(slot) => *slot,
                _ => {
                    let slot = self.allocate_temporary();
                    declarations.push(self.ir.add_expr(IrExpr::Variable {
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

        let (body, _) = crate::ir::clone_expression_dag(self.ir, inline_body);
        let local_base = self.next_temporary;
        let local_count = super::source_calls::rehome_inline_body_values(
            self.ir,
            body,
            &formal_slots,
            local_base,
        )?;
        self.next_temporary = local_base.checked_add(local_count)?;
        self.ir.inline_only_fns.insert(impl_fn);
        self.ir.functions.get_mut(impl_fn as usize)?.body = None;
        self.ir.exprs[invocation as usize] = IrExpr::Block {
            stmts: declarations,
            value: Some(body),
        };
        Some(())
    }
}

fn mark_subtree(ir: &crate::ir::IrFile, root: ExprId, marked: &mut HashSet<ExprId>) {
    let mut pending = vec![root];
    while let Some(expression) = pending.pop() {
        if !marked.insert(expression) {
            continue;
        }
        crate::ir::for_each_child(&ir.exprs, expression, &mut |child| pending.push(child));
    }
}

fn value_indices(expression: &IrExpr) -> Vec<u32> {
    match expression {
        IrExpr::GetValue(index)
        | IrExpr::SetValue { var: index, .. }
        | IrExpr::Variable { index, .. } => vec![*index],
        IrExpr::Try { catches, .. } => catches.iter().map(|catch| catch.var).collect(),
        _ => Vec::new(),
    }
}

fn rebase_index(
    index: &mut u32,
    parameter_count: u32,
    operands: &[Option<u32>],
    local_base: u32,
) -> Option<()> {
    *index = if *index < parameter_count {
        operands.get(*index as usize).copied().flatten()?
    } else {
        local_base.checked_add(*index - parameter_count)?
    };
    Some(())
}

fn rebase_values(
    expression: &mut IrExpr,
    parameter_count: u32,
    operands: &[Option<u32>],
    local_base: u32,
) -> Option<()> {
    match expression {
        IrExpr::GetValue(index)
        | IrExpr::SetValue { var: index, .. }
        | IrExpr::Variable { index, .. } => {
            rebase_index(index, parameter_count, operands, local_base)?
        }
        IrExpr::Try { catches, .. } => {
            for catch in catches {
                rebase_index(&mut catch.var, parameter_count, operands, local_base)?;
            }
        }
        _ => {}
    }
    Some(())
}

fn specialize_types(expression: &mut IrExpr, bindings: &HashMap<String, Ty>) {
    match expression {
        IrExpr::Checked(operation) => specialize_checked_operation(operation, bindings),
        IrExpr::KClassLiteral { classifier, .. } => specialize_optional_ty(classifier, bindings),
        IrExpr::LocalPropertyReference { property_type, .. } => {
            specialize_ty(property_type, bindings)
        }
        IrExpr::Call { callee, .. } => specialize_callee(callee, bindings),
        IrExpr::TypeOp {
            op, type_operand, ..
        } => {
            let declaration_generic_target = matches!(type_operand.non_null(), Ty::TyParam(..));
            specialize_ty(type_operand, bindings);
            // `as T` is checked against T's declaration bound, so an unconstrained T initially
            // admits null. Once an inline call fixes T to a concrete non-null type, the checked
            // operation has non-null cast semantics at that use site (and a primitive target must
            // subsequently unbox). This is specialization of the selected type operation, not a
            // new lookup or inference decision in lowering.
            if declaration_generic_target
                && *op == crate::ir::IrTypeOp::Cast
                && !type_operand.is_nullable()
            {
                *op = crate::ir::IrTypeOp::CastNonNull;
            }
        }
        IrExpr::Variable {
            ty: type_operand, ..
        }
        | IrExpr::PrimitiveNeg {
            ty: type_operand, ..
        }
        | IrExpr::PropertyRead {
            ty: type_operand, ..
        }
        | IrExpr::PropertyWrite {
            ty: type_operand, ..
        }
        | IrExpr::RefNew {
            elem: type_operand, ..
        }
        | IrExpr::RefGet {
            elem: type_operand, ..
        }
        | IrExpr::RefSet {
            elem: type_operand, ..
        }
        | IrExpr::Vararg {
            array_type: type_operand,
            ..
        }
        | IrExpr::NewArray {
            array_type: type_operand,
            ..
        }
        | IrExpr::Try {
            result: type_operand,
            ..
        } => specialize_ty(type_operand, bindings),
        IrExpr::New {
            ctor_params: Some(parameters),
            ..
        } => specialize_tys(parameters, bindings),
        IrExpr::InvokeFunction { params, ret, .. } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
        }
        IrExpr::Lambda { sam: Some(sam), .. } => specialize_sam_target(sam, bindings),
        IrExpr::Const(_)
        | IrExpr::ClassConst { .. }
        | IrExpr::SingletonValue { .. }
        | IrExpr::GetValue(_)
        | IrExpr::SetValue { .. }
        | IrExpr::PluginPlaceholder { .. }
        | IrExpr::Return(_)
        | IrExpr::Block { .. }
        | IrExpr::When { .. }
        | IrExpr::While { .. }
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::PrimitiveBinOp { .. }
        | IrExpr::StringConcat(_)
        | IrExpr::EnclosingInstance { .. }
        | IrExpr::GetField { .. }
        | IrExpr::LateinitInitialized { .. }
        | IrExpr::SetField { .. }
        | IrExpr::GetStatic(_)
        | IrExpr::SetStatic { .. }
        | IrExpr::New {
            ctor_params: None, ..
        }
        | IrExpr::MethodCall { .. }
        | IrExpr::EnumEntry { .. }
        | IrExpr::StaticInstance { .. }
        | IrExpr::ExternalStaticField { .. }
        | IrExpr::EnumValues { .. }
        | IrExpr::EnumValueOf { .. }
        | IrExpr::EnumEntries { .. }
        | IrExpr::ReifiedClassMarker { .. }
        | IrExpr::ReifiedTypeOp { .. }
        | IrExpr::Lambda { sam: None, .. }
        | IrExpr::UnitInstance
        | IrExpr::CurrentContinuation
        | IrExpr::NotNullAssert { .. }
        | IrExpr::LateinitCheck { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::Throw { .. } => {}
    }
}

fn specialize_expression_facts(
    ir: &mut crate::ir::IrFile,
    expression: ExprId,
    bindings: &HashMap<String, Ty>,
) {
    for ty in [
        ir.logical_types.get_mut(&expression),
        ir.exhaustive_whens.get_mut(&expression),
        ir.physical_types.get_mut(&expression),
        ir.ext_call_source_receiver.get_mut(&expression),
        ir.call_declared_ret.get_mut(&expression),
        ir.suspend_calls.get_mut(&expression),
    ]
    .into_iter()
    .flatten()
    {
        specialize_ty(ty, bindings);
    }
    if let Some(parameters) = ir.call_declared_params.get_mut(&expression) {
        specialize_tys(parameters, bindings);
    }
    if let Some(substitutions) = ir.reified_call_subst.get_mut(&expression) {
        for (_, ty) in substitutions {
            specialize_ty(ty, bindings);
        }
    }
    if let Some(construction) = ir.annotation_constructions.get_mut(&expression) {
        for (_, ty) in &mut construction.members {
            specialize_ty(ty, bindings);
        }
    }
    if let Some(result) = ir.value_class_suspend_calls.get_mut(&expression) {
        match result {
            IrValueClassSuspendResult::Boxed { carrier, .. }
            | IrValueClassSuspendResult::Carrier(carrier) => specialize_ty(carrier, bindings),
        }
    }
    if let Some(point) = ir.intrinsic_suspension_points.get_mut(&expression) {
        specialize_ty(&mut point.result, bindings);
    }
}

fn specialize_ty(ty: &mut Ty, bindings: &HashMap<String, Ty>) {
    *ty = ty_subst_keep_unbound(*ty, bindings);
}

fn specialize_optional_ty(ty: &mut Option<Ty>, bindings: &HashMap<String, Ty>) {
    if let Some(ty) = ty {
        specialize_ty(ty, bindings);
    }
}

fn specialize_tys(types: &mut [Ty], bindings: &HashMap<String, Ty>) {
    for ty in types {
        specialize_ty(ty, bindings);
    }
}

fn specialize_resolved_ty(ty: &mut ResolvedTy, bindings: &HashMap<String, Ty>) {
    *ty = ResolvedTy::new(ty_subst_keep_unbound(ty.get(), bindings))
        .expect("inline specialization preserves resolved types");
}

fn specialize_resolved_tys(types: &mut [ResolvedTy], bindings: &HashMap<String, Ty>) {
    for ty in types {
        specialize_resolved_ty(ty, bindings);
    }
}

fn specialize_checked_substitution(
    substitution: &mut IrCheckedSubstitution,
    bindings: &HashMap<String, Ty>,
) {
    specialize_ty(&mut substitution.value, bindings);
    specialize_tys(&mut substitution.additional_bounds, bindings);
}

fn specialize_checked_argument(argument: &mut IrCheckedArgument, bindings: &HashMap<String, Ty>) {
    if let IrCheckedArgument::Vararg { array_type, .. } = argument {
        specialize_ty(array_type, bindings);
    }
}

fn specialize_intrinsic(operation: &mut IrIntrinsic, bindings: &HashMap<String, Ty>) {
    match operation {
        IrIntrinsic::PrimitiveCompare { operand }
        | IrIntrinsic::UnsignedToString { source: operand }
        | IrIntrinsic::PrimitiveArrayNew { element: operand }
        | IrIntrinsic::EnumValueOf {
            classifier: operand,
        }
        | IrIntrinsic::DataClassFieldEquals { ty: operand }
        | IrIntrinsic::DataClassFieldHash { ty: operand }
        | IrIntrinsic::DataClassArrayToString { ty: operand } => specialize_ty(operand, bindings),
        IrIntrinsic::Assert { .. }
        | IrIntrinsic::ArrayGet
        | IrIntrinsic::ArraySet
        | IrIntrinsic::ArraySize
        | IrIntrinsic::StringGet
        | IrIntrinsic::StringLength
        | IrIntrinsic::StringPlus
        | IrIntrinsic::NullableAnyToString
        | IrIntrinsic::CoroutineContext => {}
    }
}

fn specialize_callee(callee: &mut Callee, bindings: &HashMap<String, Ty>) {
    match callee {
        Callee::Intrinsic { operation, ret } => {
            specialize_intrinsic(operation, bindings);
            specialize_ty(ret, bindings);
        }
        Callee::CrossFile { params, ret, .. }
        | Callee::Module { params, ret, .. }
        | Callee::Super { params, ret, .. } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
        }
        Callee::External {
            params,
            ret,
            substitutions,
            ..
        } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
        }
        Callee::Virtual {
            params: Some((params, ret)),
            ..
        } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
        }
        Callee::Local(_)
        | Callee::ClassStatic { .. }
        | Callee::ClassStaticDefault { .. }
        | Callee::LocalDefault(_)
        | Callee::Static { .. }
        | Callee::Virtual { params: None, .. }
        | Callee::Special { .. } => {}
    }
}

fn specialize_sam_target(target: &mut IrSamTarget, bindings: &HashMap<String, Ty>) {
    specialize_tys(&mut target.parameters, bindings);
    specialize_ty(&mut target.result, bindings);
    specialize_tys(&mut target.declared_parameters, bindings);
    specialize_ty(&mut target.declared_result, bindings);
}

fn specialize_reference_adaptation(
    adaptation: &mut Option<Box<FirReferenceAdaptation>>,
    bindings: &HashMap<String, Ty>,
) {
    let Some(adaptation) = adaptation else {
        return;
    };
    specialize_resolved_tys(&mut adaptation.parameter_types, bindings);
    specialize_resolved_ty(&mut adaptation.result_type, bindings);
}

fn specialize_constructor_target(
    target: &mut FirConstructorTarget,
    bindings: &HashMap<String, Ty>,
) {
    if let FirConstructorTarget::External {
        parameters,
        annotation,
        ..
    } = target
    {
        specialize_resolved_tys(parameters, bindings);
        if let Some(annotation) = annotation {
            for (_, ty) in &mut annotation.members {
                specialize_resolved_ty(ty, bindings);
            }
        }
    }
}

fn specialize_callable_reference_target(
    target: &mut FirCallableReferenceTarget,
    bindings: &HashMap<String, Ty>,
) {
    match target {
        FirCallableReferenceTarget::Module(_) => {}
        FirCallableReferenceTarget::ArrayFactory {
            array_type,
            element_type,
            parameters,
            ..
        } => {
            specialize_resolved_ty(array_type, bindings);
            specialize_resolved_ty(element_type, bindings);
            specialize_resolved_tys(parameters, bindings);
        }
        FirCallableReferenceTarget::Constructor {
            target,
            outer,
            parameters,
            result,
            ..
        } => {
            specialize_constructor_target(target, bindings);
            if let Some(outer) = outer {
                specialize_resolved_ty(outer, bindings);
            }
            specialize_resolved_tys(parameters, bindings);
            specialize_resolved_ty(result, bindings);
        }
        FirCallableReferenceTarget::External {
            receiver,
            parameters,
            result,
            ..
        } => {
            if let Some(receiver) = receiver {
                specialize_resolved_ty(receiver, bindings);
            }
            specialize_resolved_tys(parameters, bindings);
            specialize_resolved_ty(result, bindings);
        }
        FirCallableReferenceTarget::Classifier {
            parameters, result, ..
        } => {
            specialize_resolved_tys(parameters, bindings);
            specialize_resolved_ty(result, bindings);
        }
    }
}

fn specialize_property_target(target: &mut FirPropertyTarget, bindings: &HashMap<String, Ty>) {
    if let FirPropertyTarget::External {
        receiver,
        parameters,
        result,
        ..
    } = target
    {
        if let Some(receiver) = receiver {
            specialize_resolved_ty(receiver, bindings);
        }
        specialize_resolved_tys(parameters, bindings);
        specialize_resolved_ty(result, bindings);
    }
}

fn specialize_property_reference_target(
    target: &mut FirPropertyReferenceTarget,
    bindings: &HashMap<String, Ty>,
) {
    match target {
        FirPropertyReferenceTarget::Module(_) => {}
        FirPropertyReferenceTarget::SpecializedModule {
            receiver,
            property_type,
            ..
        } => {
            if let Some(receiver) = receiver {
                specialize_resolved_ty(receiver, bindings);
            }
            specialize_resolved_ty(property_type, bindings);
        }
        FirPropertyReferenceTarget::Classifier { property_type, .. } => {
            specialize_resolved_ty(property_type, bindings)
        }
        FirPropertyReferenceTarget::External {
            reflection_owner,
            getter,
            setter,
            property_type,
            ..
        } => {
            if let Some(owner) = reflection_owner {
                specialize_resolved_ty(owner, bindings);
            }
            specialize_property_target(getter, bindings);
            if let Some(setter) = setter {
                specialize_property_target(setter, bindings);
            }
            specialize_resolved_ty(property_type, bindings);
        }
    }
}

fn specialize_checked_operation(
    operation: &mut IrCheckedOperation,
    bindings: &HashMap<String, Ty>,
) {
    match operation {
        IrCheckedOperation::Call {
            arguments,
            substitutions,
            ..
        } => {
            for argument in arguments {
                specialize_checked_argument(argument, bindings);
            }
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
        }
        IrCheckedOperation::ConstructorDelegation {
            target,
            outer_parameter,
            arguments,
            substitutions,
            ..
        } => {
            if let crate::ir::IrCheckedConstructorTarget::External { parameters, .. } = target {
                specialize_tys(parameters, bindings);
            }
            specialize_optional_ty(outer_parameter, bindings);
            for argument in arguments {
                specialize_checked_argument(argument, bindings);
            }
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
        }
        IrCheckedOperation::PropertyRead { substitutions, .. }
        | IrCheckedOperation::PropertyWrite { substitutions, .. } => {
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
        }
        IrCheckedOperation::ExternalPropertyRead {
            parameters,
            result,
            source_receiver,
            ..
        }
        | IrCheckedOperation::ExternalPropertyWrite {
            parameters,
            result,
            source_receiver,
            ..
        } => {
            specialize_tys(parameters, bindings);
            specialize_ty(result, bindings);
            specialize_optional_ty(source_receiver, bindings);
        }
        IrCheckedOperation::RangeConstruction {
            start_type,
            end_type,
            result,
            ..
        } => {
            specialize_ty(start_type, bindings);
            specialize_ty(end_type, bindings);
            specialize_ty(result, bindings);
        }
        IrCheckedOperation::RangeContains { counter, .. }
        | IrCheckedOperation::RangeLoop { counter, .. } => specialize_ty(counter, bindings),
        IrCheckedOperation::CallableReference {
            target,
            function_type,
            substitutions,
            adaptation,
            ..
        } => {
            specialize_callable_reference_target(target, bindings);
            specialize_ty(function_type, bindings);
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
            specialize_reference_adaptation(adaptation, bindings);
        }
        IrCheckedOperation::PropertyReference {
            target,
            substitutions,
            adaptation,
            ..
        } => {
            specialize_property_reference_target(target, bindings);
            for substitution in substitutions {
                specialize_checked_substitution(substitution, bindings);
            }
            specialize_reference_adaptation(adaptation, bindings);
        }
        IrCheckedOperation::LateinitFieldRead { .. }
        | IrCheckedOperation::BackingFieldRead { .. }
        | IrCheckedOperation::BackingFieldWrite { .. } => {}
    }
}
