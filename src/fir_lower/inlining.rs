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
    Callee, ExprId, IrCheckedArgument, IrCheckedOperation, IrCheckedSubstitution, IrConst,
    IrDebugLocalProvenance, IrExpr, IrInlineLocalRole, IrIntrinsic, IrSamTarget,
    IrValueClassSuspendResult,
};
use crate::types::{stored_value_ty, ty_subst_keep_unbound, Ty};

use super::BodyLowering;

/// What one physical operand position of an inline expansion fills.
enum InlineOperandRole {
    /// A receiver. It declares no value parameter, and kotlinc gives it a local of its own
    /// (`$this$…`) exactly as it does for an ordinary value.
    Receiver,
    /// A declared value parameter that wrote `noinline`: its argument is a real closure with its
    /// own local, name and lifetime.
    Materialized,
    /// A declared value parameter the callee expands at each of its uses. It owns no local, so an
    /// argument that is already a local keeps the caller's slot.
    Spliced,
    /// A declared value parameter whose role the index never published. Neither answer above can
    /// be assumed from what is left: the expansion declines.
    Unpublished,
}

/// One physical operand position of an inline expansion: the local it would materialize, and what
/// the callee says that position is.
struct InlineOperand {
    source_name: Option<String>,
    local_role: IrInlineLocalRole,
    role: InlineOperandRole,
}

/// What the expansion will do with one operand, decided before anything is allocated.
enum InlineOperandPlan {
    /// The checker recorded an inline lambda for this parameter. Every reference to it inside the
    /// body is replaced by that lambda's own tree, so the parameter takes no slot at all.
    Splice,
    /// Keep the caller's slot: the parameter is one the callee expands at each of its uses, so it
    /// owns no local of its own and an argument that is already a local needs no copy.
    Reuse(u32),
    /// Copy the argument into a local belonging to this expansion.
    Copy,
}

impl InlineOperand {
    fn receiver(name: String, local_role: IrInlineLocalRole) -> Self {
        Self {
            source_name: Some(name),
            local_role,
            role: InlineOperandRole::Receiver,
        }
    }

    fn is_unpublished(&self) -> bool {
        matches!(self.role, InlineOperandRole::Unpublished)
    }

    fn is_spliced(&self) -> bool {
        matches!(self.role, InlineOperandRole::Spliced)
    }
}

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
        // Only a reified parameter's argument is known inside the expanded body at run time. A
        // dependency's reified operation that mentions an ordinary parameter of this declaration
        // keeps that parameter, exactly as the declaration's own emitted body would: `typeOf`
        // describes it as a type parameter rather than as whatever this call site happened to
        // infer for it.
        let reified_bindings = substitutions
            .iter()
            .filter_map(|substitution| match substitution.parameter {
                FirTypeParameterRef::Module(parameter) => self
                    .index
                    .type_parameter_header(parameter)
                    .filter(|header| header.flags.is_reified())
                    .and_then(|_| self.index.type_parameter_semantic_name(parameter))
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
        // Preserve the source name and semantic role/depth of every local this expansion
        // materializes. Target-specific decoration is deferred until debug-info emission.
        let mut parameter_names: Vec<InlineOperand> = Vec::new();
        if self
            .ir
            .functions
            .get(function as usize)?
            .dispatch_receiver
            .is_some()
        {
            parameter_names.push(InlineOperand::receiver(
                self.ir.functions[function as usize].name.clone(),
                IrInlineLocalRole::DispatchReceiver,
            ));
        }
        if let Some(identities) = self.ir.function_parameter_identities(function) {
            let mut ordinal = 0;
            for identity in identities {
                if matches!(identity.role, crate::ir::IrParameterRole::ExtensionReceiver) {
                    parameter_names.push(InlineOperand::receiver(
                        self.ir.functions[function as usize].name.clone(),
                        IrInlineLocalRole::ExtensionReceiver,
                    ));
                    continue;
                }
                parameter_names.push(InlineOperand {
                    source_name: identity.source_name.clone(),
                    local_role: IrInlineLocalRole::Value,
                    // `noinline` is the callee's own statement that this argument is a real
                    // closure rather than a body spliced at each use.
                    role: match self
                        .index
                        .callable_parameter(target, ordinal)
                        .map(|parameter| parameter.flags().materializes_its_lambda())
                    {
                        Some(true) => InlineOperandRole::Materialized,
                        Some(false) => InlineOperandRole::Spliced,
                        None => InlineOperandRole::Unpublished,
                    },
                });
                ordinal += 1;
            }
        }
        // Decide every operand BEFORE anything is allocated. A declined expansion must leave the
        // arena exactly as it found it: a copy made for a parameter the expansion then refuses is
        // an orphan node, and a temporary allocated for it shifts every local index after it.
        //
        // An argument that is already a local read still becomes a local OF THE EXPANSION: kotlinc
        // copies it so the inline parameter has its own identity, name and lifetime. Reusing the
        // caller's slot silently erased the parameter.
        //
        // A SPLICED function-typed argument is the exception, and keeps the caller's slot: such a
        // parameter is expanded at each of its call sites rather than stored, so it has no local of
        // its own to name. Copying one both invents a local kotlinc has no counterpart for and
        // hides the lambda from the splicer — a forwarded `p` (`inline fun block(p: () -> Unit) =
        // blockImpl(p)`) then materialized a `Function0` whose implementation method was never
        // emitted.
        //
        // The function TYPE alone does not say which is which. A `noinline` parameter is
        // function-typed exactly like the spliced one beside it and is a real closure with its own
        // local, name and lifetime, so the callee's declared role decides and the type only rules
        // out the parameters that cannot splice at all.
        //
        // A parameter with no name or no published role to align against is a broken contract
        // between this expansion and the callable's published header, not a shape to fall back on:
        // either answer silently erases something — the parameter's identity, or the splice.
        let mut plans = Vec::with_capacity(operands.len());
        for (index, ((operand, lambda), ty)) in operands
            .iter()
            .zip(inline_lambdas)
            .zip(&operand_types)
            .enumerate()
        {
            plans.push(match (self.ir.expr(*operand), lambda) {
                (IrExpr::GetValue(_), None)
                    if matches!(ty.non_null(), crate::types::Ty::Fun(_))
                        && parameter_names
                            .get(index)
                            .is_some_and(InlineOperand::is_unpublished) =>
                {
                    return None
                }
                (IrExpr::GetValue(_), None) if parameter_names.get(index).is_none() => return None,
                (IrExpr::GetValue(slot), None)
                    if matches!(ty.non_null(), crate::types::Ty::Fun(_))
                        && parameter_names
                            .get(index)
                            .is_some_and(InlineOperand::is_spliced) =>
                {
                    InlineOperandPlan::Reuse(*slot)
                }
                (_, None) => InlineOperandPlan::Copy,
                (_, Some(_)) => InlineOperandPlan::Splice,
            });
        }
        let mut operand_declarations = Vec::new();
        let operand_slots = plans
            .into_iter()
            .zip(operands.iter())
            .zip(&operand_types)
            .enumerate()
            .map(|(index, ((plan, operand), ty))| match plan {
                InlineOperandPlan::Splice => None,
                InlineOperandPlan::Reuse(slot) => Some(slot),
                InlineOperandPlan::Copy => {
                    let slot = self.allocate_temporary();
                    let declaration = self.ir.add_expr(IrExpr::Variable {
                        index: slot,
                        ty: stored_value_ty(*ty),
                        init: Some(*operand),
                        named: true,
                    });
                    self.ir.call_operand_bindings.insert(declaration);
                    if let Some(parameter) = parameter_names.get(index) {
                        if let Some(source_name) = parameter.source_name.clone() {
                            self.ir.value_names.insert(declaration, source_name);
                        }
                        self.ir.set_debug_local_provenance(
                            declaration,
                            IrDebugLocalProvenance::inline_value(parameter.local_role, 1),
                        );
                    }
                    operand_declarations.push(declaration);
                    Some(slot)
                }
            })
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

        // Every return this expansion contains, COLLECTED but not yet rewritten. Which shape the
        // expansion takes is decided from this list, and only the loop shape costs a result local
        // and a break per return — so nothing is reserved or allocated until the shape is known.
        // Reserving first left a hole in the local numbering and a pair of unreachable arena nodes
        // behind every successful tail promotion, which shifts every later local identity.
        let mut returns: Vec<(ExprId, Option<ExprId>)> = Vec::new();

        // Clone order, not map order: rewriting the copies allocates locals and records returns,
        // and both orders reach the emitted code.
        let mut copies = cloned
            .iter()
            .map(|(&source, &copy)| (source, copy))
            .collect::<Vec<_>>();
        copies.sort_by_key(|&(_, copy)| copy);
        for &(source, copy) in &copies {
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
            specialize_dependency_substitutions(
                self.ir.exprs.get_mut(copy as usize)?,
                &reified_bindings,
            );
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
            // A cloned declaration keeps its source name and gains one inline frame. Cloning has
            // already copied any existing provenance, so nested expansion increments the typed
            // depth instead of parsing a previously formatted target name.
            if self.ir.value_names.contains_key(&source) {
                let provenance = self
                    .ir
                    .debug_local_provenance(source)
                    .map(IrDebugLocalProvenance::nested_inline)
                    .unwrap_or_else(|| {
                        IrDebugLocalProvenance::inline_value(IrInlineLocalRole::Value, 1)
                    });
                self.ir.set_debug_local_provenance(copy, provenance);
            }
            // A `catch` binding is declared by its `IrCatch` rather than by a `Variable` node, so
            // it carries the same two facts in the record that declares it and gains its frame
            // here rather than through the tables above.
            if let Some(IrExpr::Try { catches, .. }) = self.ir.exprs.get_mut(copy as usize) {
                for catch in catches {
                    if let Some(binding) = catch.binding.as_mut() {
                        binding.nest_inline();
                    }
                }
            }
            if protected.contains(&source) {
                continue;
            }
            if let IrExpr::GetValue(parameter) = self.ir.expr(source) {
                if let Some(Some(lambda)) = inline_lambdas.get(*parameter as usize) {
                    self.ir.exprs[copy as usize] = self.ir.expr(*lambda).clone();
                    self.ir.binding_read_stability.remove(&copy);
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
                returns.push((copy, value));
                // This return has crossed its checked callable boundary and will be represented by
                // the block that replaces it, in EITHER shape. The sparse depth fact belongs to the
                // old `Return` node; leaving it on the replacement makes an enclosing inline-lambda
                // template try to consume the same return a second time. Removing it is a fact
                // about the node, not a commitment to the loop shape, so it happens here.
                self.ir.checked_return_depths.remove(&copy);
            }
        }

        let inline_invocations = copies
            .iter()
            .map(|&(_, copy)| copy)
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

        // An expansion whose ONLY return is its tail needs neither a result local nor the loop that
        // carries a non-local return out: the value is simply the body's value, which is what kotlinc
        // emits — it leaves it on the operand stack. The loop form costs an unnamed local, and when
        // the expansion crosses a suspension that local takes a continuation field kotlinc has no
        // counterpart for.
        if let [(tail, value)] = returns[..] {
            if produce_sole_tail_return(self.ir, cloned_root, tail, value) {
                let mut statements = operand_declarations;
                statements.push(cloned_root);
                let value = statements.pop();
                return Some(self.ir.add_expr(IrExpr::Block {
                    stmts: statements,
                    value,
                }));
            }
        }

        // The loop shape, and only now: the result local is reserved here, so a promoted expansion
        // above leaves no hole in the numbering, and each return is rewritten into the break that
        // carries it out.
        let result_slot = (result_ty != Ty::Unit).then(|| {
            let slot = self.next_temporary;
            self.next_temporary += 1;
            slot
        });
        let label = format!("$fir_inline${}_{}", target.raw(), self.next_temporary);
        for (copy, value) in returns {
            let exit = self.ir.add_expr(IrExpr::Break {
                label: Some(label.clone()),
            });
            self.ir.exprs[copy as usize] = if let (Some(slot), Some(value)) = (result_slot, value) {
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

        let receiver_parameter = self
            .ir
            .lambda_origins
            .get(&impl_fn)
            .and_then(|origin| origin.receiver_parameter);
        let mut declarations = Vec::new();
        let mut formal_slots = Vec::with_capacity(parameter_types.len());
        // A lambda's own VALUE parameters are locals of the splice and keep their source names, so a
        // suspension inside the body spills them under those names. Captures are not: they are the
        // enclosing locals, already named where they were declared. No provenance is attached here —
        // a lambda written at source level renders its parameter bare, and cloning the body into an
        // enclosing expansion is what raises the typed inline depth the JVM boundary formats.
        let capture_count = captures.len();
        let lambda_parameter_identities = self
            .ir
            .function_parameter_identities(impl_fn)
            .map(<[crate::ir::IrParameterIdentity]>::to_vec);
        for (parameter, (value, ty)) in captures
            .into_iter()
            .chain(args)
            .zip(parameter_types)
            .enumerate()
        {
            let parameter = u32::try_from(parameter).ok()?;
            let slot = if receiver_parameter == Some(parameter) {
                let slot = self.allocate_temporary();
                let declaration = self.ir.add_expr(IrExpr::Variable {
                    index: slot,
                    ty,
                    init: Some(value),
                    named: true,
                });
                self.ir.call_operand_bindings.insert(declaration);
                self.ir.set_debug_local_provenance(
                    declaration,
                    IrDebugLocalProvenance::InlineLambdaReceiver {
                        implementation: impl_fn,
                    },
                );
                declarations.push(declaration);
                slot
            } else {
                let source_name = (parameter as usize >= capture_count)
                    .then(|| {
                        lambda_parameter_identities
                            .as_ref()
                            .and_then(|identities| identities.get(parameter as usize))
                            .and_then(|identity| identity.source_name.clone())
                    })
                    .flatten();
                match (self.ir.expr(value), &source_name) {
                    (IrExpr::GetValue(slot), None) => *slot,
                    _ => {
                        let slot = self.allocate_temporary();
                        let declaration = self.ir.add_expr(IrExpr::Variable {
                            index: slot,
                            ty,
                            init: Some(value),
                            named: source_name.is_some(),
                        });
                        self.ir.call_operand_bindings.insert(declaration);
                        if let Some(name) = source_name {
                            self.ir.value_names.insert(declaration, name);
                        }
                        declarations.push(declaration);
                        slot
                    }
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
        IrExpr::Checked(IrCheckedOperation::RangeLoop { variable, .. }) => vec![*variable],
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
        // A parameter with no slot is one an inline lambda supplies. Every reference to it was
        // replaced by that lambda before rebasing, so reaching one here is a reference the
        // replacement did not see and the expansion cannot be completed.
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
        IrExpr::Checked(IrCheckedOperation::RangeLoop { variable, .. }) => {
            rebase_index(variable, parameter_count, operands, local_base)?;
        }
        _ => {}
    }
    Some(())
}

fn specialize_types(expression: &mut IrExpr, bindings: &HashMap<String, Ty>) {
    match expression {
        IrExpr::Checked(operation) => specialize_checked_operation(operation, bindings),
        IrExpr::CallableReference(reference) => {
            specialize_ty(&mut reference.function_type, bindings);
            specialize_tys(&mut reference.declaration_parameters, bindings);
            specialize_ty(&mut reference.declaration_result, bindings);
            specialize_reference_adaptation(&mut reference.adaptation, bindings);
        }
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
        IrExpr::PluginPlaceholder { types, .. } => specialize_tys(types, bindings),
        IrExpr::Lambda { sam: Some(sam), .. } => specialize_sam_target(sam, bindings),
        IrExpr::Const(_)
        | IrExpr::ClassConst { .. }
        | IrExpr::SingletonValue { .. }
        | IrExpr::GetValue(_)
        | IrExpr::SetValue { .. }
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
        | IrExpr::BottomValue { .. }
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
    if let Some(parameters) = ir.construction_declared_params.get_mut(&expression) {
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
        IrIntrinsic::PrimitiveCompare { operand, .. }
        | IrIntrinsic::UnsignedToString { source: operand }
        | IrIntrinsic::PrimitiveArrayNew { element: operand }
        | IrIntrinsic::EnumValueOf {
            classifier: operand,
        }
        | IrIntrinsic::DataClassFieldEquals { ty: operand }
        | IrIntrinsic::DataClassFieldHash { ty: operand }
        | IrIntrinsic::DataClassArrayToString { ty: operand }
        | IrIntrinsic::TypeOf { ty: operand } => specialize_ty(operand, bindings),
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
        Callee::ModuleWithDefaults {
            params,
            ret,
            dispatch_receiver_ty,
            ..
        } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
            if let Some(receiver) = dispatch_receiver_ty {
                specialize_ty(receiver, bindings);
            }
        }
        // The dependency's own substitutions are specialized separately, by the reified
        // arguments alone (`specialize_dependency_substitutions`).
        Callee::External { params, ret, .. } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
        }
        Callee::Virtual {
            params: Some((params, ret)),
            ..
        } => {
            specialize_tys(params, bindings);
            specialize_ty(ret, bindings);
        }
        Callee::Local(_)
        | Callee::LocalWithDefaults { .. }
        | Callee::ClassStatic { .. }
        | Callee::ClassStaticWithDefaults { .. }
        | Callee::ClassStaticDefault { .. }
        | Callee::LocalDefault(_)
        | Callee::Static { .. }
        | Callee::Virtual { params: None, .. }
        | Callee::Special { .. } => {}
    }
}

/// Specialize the type arguments a dependency call selected. Only these reach a dependency's
/// reified operations, which see the reified arguments of the expanded declaration and nothing
/// else.
fn specialize_dependency_substitutions(
    expression: &mut IrExpr,
    reified_bindings: &HashMap<String, Ty>,
) {
    if let IrExpr::Call {
        callee: Callee::External { substitutions, .. },
        ..
    } = expression
    {
        for substitution in substitutions {
            specialize_checked_substitution(substitution, reified_bindings);
        }
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

/// Rewrite `root` to PRODUCE the value of its sole rewritten return, or leave it exactly as it was.
///
/// The shape is PROVED before anything changes, and proving it cannot change anything: the chain of
/// statement blocks from `root` down to `tail` is collected from a shared reference. Only once that
/// answers does the `Unit` placeholder get allocated and the blocks rewritten. Ordering it the other
/// way left an orphan `UnitInstance` in the arena whenever the answer turned out to be no — the
/// block shapes were restored, but the allocation was not, so a refused optimization still shifted
/// every expression identity after it.
fn produce_sole_tail_return(
    ir: &mut crate::ir::IrFile,
    root: ExprId,
    tail: ExprId,
    value: Option<ExprId>,
) -> bool {
    let Some(chain) = tail_statement_block_chain(ir, root, tail) else {
        return false;
    };
    let produced = value.unwrap_or_else(|| ir.add_expr(IrExpr::UnitInstance));
    ir.exprs[tail as usize] = IrExpr::Block {
        stmts: Vec::new(),
        value: Some(produced),
    };
    for &block in &chain {
        let IrExpr::Block { stmts, value: None } = ir.expr(block).clone() else {
            unreachable!("the chain was proved to be statement blocks")
        };
        let mut stmts = stmts;
        let last = stmts.pop().expect("a chained block ends in a statement");
        ir.exprs[block as usize] = IrExpr::Block {
            stmts,
            value: Some(last),
        };
    }
    true
}

/// The statement blocks from `block` down to `tail`, outermost first — the ones that must become
/// value-producing for `tail`'s value to reach `block`.
///
/// `None` unless the path is a chain of statement blocks each ending in the next, which is every
/// case that has to keep the caller's result local and labelled exit loop: a `return` anywhere but
/// the tail must be able to carry its value out of the middle of the body.
fn tail_statement_block_chain(
    ir: &crate::ir::IrFile,
    block: ExprId,
    tail: ExprId,
) -> Option<Vec<ExprId>> {
    let mut chain = Vec::new();
    let mut current = block;
    loop {
        let IrExpr::Block { stmts, value: None } = ir.expr(current) else {
            return None;
        };
        let &last = stmts.last()?;
        chain.push(current);
        if last == tail {
            return Some(chain);
        }
        current = last;
    }
}

#[cfg(test)]
mod value_rebasing_tests {
    use super::{rebase_values, value_indices};
    use crate::fir::FirRangeOperation;
    use crate::ir::{IrCheckedOperation, IrExpr};
    use crate::types::Ty;

    #[test]
    fn a_checked_range_loop_reserves_and_rebases_its_declared_value() {
        let mut expression = IrExpr::Checked(IrCheckedOperation::RangeLoop {
            variable: 4,
            variable_name: Some("element".into()),
            counter: Ty::Int,
            operation: FirRangeOperation::Until,
            start: 0,
            end: 1,
            body: 2,
            label: "loop".to_string(),
        });

        assert_eq!(value_indices(&expression), vec![4]);
        assert_eq!(rebase_values(&mut expression, 2, &[], 20), Some(()));
        assert_eq!(value_indices(&expression), vec![22]);
    }
}

#[cfg(test)]
mod tail_promotion_tests {
    use super::{produce_sole_tail_return, tail_statement_block_chain};
    use crate::ir::{ExprId, IrConst, IrExpr, IrFile};

    fn statement(ir: &mut IrFile, value: i32) -> ExprId {
        ir.add_expr(IrExpr::Const(IrConst::Int(value)))
    }

    fn statement_block(ir: &mut IrFile, stmts: Vec<ExprId>) -> ExprId {
        ir.add_expr(IrExpr::Block { stmts, value: None })
    }

    /// Everything the arena holds, as text: both its LENGTH and every node, so a refusal that
    /// allocated or replaced anything at all shows up.
    fn arena(ir: &IrFile) -> String {
        format!("{} nodes: {:?}", ir.exprs.len(), ir.exprs)
    }

    fn assert_block(ir: &IrFile, block: ExprId, stmts: &[ExprId], value: Option<ExprId>) {
        let IrExpr::Block {
            stmts: actual,
            value: produced,
        } = ir.expr(block)
        else {
            panic!("expression {block} is not a block");
        };
        assert_eq!(actual.as_slice(), stmts, "block {block} statements");
        assert_eq!(*produced, value, "block {block} value");
    }

    /// The direct case: the expansion's body IS the block whose last statement is the tail.
    #[test]
    fn a_direct_tail_statement_becomes_the_blocks_value() {
        let mut ir = IrFile::default();
        let first = statement(&mut ir, 1);
        let returned = statement(&mut ir, 2);
        let tail = statement_block(&mut ir, vec![]);
        let block = statement_block(&mut ir, vec![first, tail]);

        assert!(produce_sole_tail_return(
            &mut ir,
            block,
            tail,
            Some(returned)
        ));
        assert_block(&ir, block, &[first], Some(tail));
        assert_block(&ir, tail, &[], Some(returned));
    }

    /// A statement-bodied inline function wraps its body one level deeper, so the promotion has to
    /// descend — and every block on that path becomes value-producing, or the value is discarded by
    /// whichever block above it still ends in a statement.
    #[test]
    fn a_nested_tail_statement_promotes_every_block_on_its_path() {
        let mut ir = IrFile::default();
        let first = statement(&mut ir, 1);
        let returned = statement(&mut ir, 2);
        let tail = statement_block(&mut ir, vec![]);
        let inner = statement_block(&mut ir, vec![first, tail]);
        let outer = statement_block(&mut ir, vec![inner]);

        assert!(produce_sole_tail_return(
            &mut ir,
            outer,
            tail,
            Some(returned)
        ));
        assert_block(&ir, outer, &[], Some(inner));
        assert_block(&ir, inner, &[first], Some(tail));
        assert_block(&ir, tail, &[], Some(returned));
    }

    /// The return is not the tail, which is the case that must keep the caller's result local and
    /// labelled exit loop — a non-local return has to be able to carry its value out of the middle
    /// of the body. Nothing may be left half-promoted when the answer is no.
    #[test]
    fn a_statement_after_the_return_changes_nothing() {
        let mut ir = IrFile::default();
        let returned = statement(&mut ir, 1);
        let tail = statement_block(&mut ir, vec![]);
        let after = statement(&mut ir, 2);
        let block = statement_block(&mut ir, vec![tail, after]);
        let before = arena(&ir);

        assert!(!produce_sole_tail_return(
            &mut ir,
            block,
            tail,
            Some(returned)
        ));
        assert_eq!(arena(&ir), before, "a refused promotion mutates nothing");
    }

    /// The `Unit` case, which is the one that used to allocate before it knew the answer: there is
    /// no returned expression, so the promotion has to materialize `Unit` itself. A refusal must
    /// leave the arena at exactly its previous LENGTH — the block shapes being restored is not
    /// enough, because an orphan allocation shifts every later expression identity.
    #[test]
    fn a_refused_unit_return_allocates_nothing() {
        let mut ir = IrFile::default();
        let tail = statement_block(&mut ir, vec![]);
        let after = statement(&mut ir, 1);
        let block = statement_block(&mut ir, vec![tail, after]);
        let before = arena(&ir);

        assert!(!produce_sole_tail_return(&mut ir, block, tail, None));
        assert_eq!(
            arena(&ir),
            before,
            "a refused Unit promotion allocates no placeholder"
        );
    }

    /// Refusal deep in the path also leaves every block above it untouched: the chain is proved
    /// before anything is written, so a failure below happens before any of them is reached.
    #[test]
    fn a_refusal_below_leaves_the_blocks_above_untouched() {
        let mut ir = IrFile::default();
        let returned = statement(&mut ir, 1);
        let tail = statement_block(&mut ir, vec![]);
        let after = statement(&mut ir, 2);
        let inner = statement_block(&mut ir, vec![tail, after]);
        let outer = statement_block(&mut ir, vec![inner]);
        let before = arena(&ir);

        assert!(!produce_sole_tail_return(
            &mut ir,
            outer,
            tail,
            Some(returned)
        ));
        assert_eq!(arena(&ir), before, "a refused promotion mutates nothing");
    }

    /// A block that already produces a value is not a statement block, so there is no tail
    /// statement to promote.
    #[test]
    fn a_value_producing_block_changes_nothing() {
        let mut ir = IrFile::default();
        let returned = statement(&mut ir, 1);
        let tail = statement_block(&mut ir, vec![]);
        let produced = statement(&mut ir, 2);
        let block = ir.add_expr(IrExpr::Block {
            stmts: vec![tail],
            value: Some(produced),
        });
        let before = arena(&ir);

        assert!(!produce_sole_tail_return(
            &mut ir,
            block,
            tail,
            Some(returned)
        ));
        assert_eq!(arena(&ir), before, "a refused promotion mutates nothing");
    }

    /// The proof itself takes a SHARED reference, so it cannot write even in principle. Asserted
    /// here as a contract rather than left to the signature alone.
    #[test]
    fn the_chain_is_proved_without_touching_the_arena() {
        let mut ir = IrFile::default();
        let first = statement(&mut ir, 1);
        let tail = statement_block(&mut ir, vec![]);
        let inner = statement_block(&mut ir, vec![first, tail]);
        let outer = statement_block(&mut ir, vec![inner]);
        let before = arena(&ir);

        assert_eq!(
            tail_statement_block_chain(&ir, outer, tail),
            Some(vec![outer, inner])
        );
        assert_eq!(arena(&ir), before, "proving the chain reads only");
    }
}
