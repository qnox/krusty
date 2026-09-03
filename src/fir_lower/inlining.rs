//! Call-site expansion of retained same-module inline bodies.
//!
//! Pass 1 retains checked FIR only for semantic inline declarations. The file sink lowers those
//! bodies before ordinary callers; this module clones that checked common-IR template at a call,
//! applies the checker's type substitutions, rebases body-local values, and turns inline-function
//! returns into an expression-local loop exit.

use std::collections::{HashMap, HashSet};

use crate::fir::{CallableId, FirTypeParameterRef, FirTypeSubstitution};
use crate::ir::{ExprId, IrConst, IrExpr};
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
        let operand_types = function_shape
            .dispatch_receiver
            .map(Ty::obj_name)
            .into_iter()
            .chain(function_shape.params.iter().copied())
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
            if let Some(logical) = self.ir.logical_types.get_mut(&copy) {
                *logical = ty_subst_keep_unbound(*logical, &bindings);
            }
            specialize_types(self.ir.exprs.get_mut(copy as usize)?, &bindings);
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
        IrExpr::TypeOp { type_operand, .. } => {
            *type_operand = ty_subst_keep_unbound(*type_operand, bindings)
        }
        IrExpr::KClassLiteral {
            classifier: Some(classifier),
            ..
        } => *classifier = ty_subst_keep_unbound(*classifier, bindings),
        _ => {}
    }
}
