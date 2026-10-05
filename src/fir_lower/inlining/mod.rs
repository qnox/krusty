//! Call-site expansion of retained same-module inline bodies.
//!
//! Pass 1 retains checked FIR only for semantic inline declarations. The file sink lowers those
//! bodies before ordinary callers; this module clones that checked common-IR template at a call,
//! applies the checker's type substitutions, rebases body-local values, and turns inline-function
//! returns into an expression-local loop exit.

use std::collections::{HashMap, HashSet};

use crate::fir::{
    CallableId, FirPropertyReferenceTarget, FirPropertyTarget, FirReferenceAdaptation,
    FirTypeParameterRef, FirTypeSubstitution, ResolvedTy,
};
use crate::ir::{
    Callee, ExprId, IrCheckedArgument, IrCheckedOperation, IrCheckedSubstitution, IrConst,
    IrDebugLocalProvenance, IrExpr, IrInlineLocalRole, IrIntrinsic, IrSamTarget,
    IrValueClassSuspendResult,
};
use crate::types::{stored_value_ty, ty_subst_keep_unbound, Ty};

use super::BodyLowering;

mod converted_inline_lambda;
mod escaping_anonymous;
mod escaping_lambda;
mod property_accessors;

#[cfg(test)]
mod escaping_reified_lambda;
#[cfg(test)]
mod escaping_reified_object;

pub(super) fn splice_inline_property_accessors(
    ir: &mut crate::ir::IrFile,
) -> Result<(), super::FirFileLoweringFailure> {
    property_accessors::splice_inline_property_accessors(ir)
}

pub(super) fn publish_reified_anonymous_accessors(
    ir: &mut crate::ir::IrFile,
) -> Result<(), super::FirFileLoweringFailure> {
    escaping_anonymous::publish_accessors(ir).map_err(super::FirFileLoweringFailure::Body)
}

/// Specialize one expression copied across an inline boundary. A local delegated-property access
/// keeps the checker-selected declaration plan: kotlinc's local-delegate helper remains the erased
/// declaration helper and is reused (or rehomed by the JVM) across call-site substitutions.
pub(super) fn specialize_inline_copy(
    ir: &mut crate::ir::IrFile,
    expression: ExprId,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
) -> Option<()> {
    specialize_recorded_facts(ir, expression, bindings, runtime);
    {
        let expression = ir.exprs.get_mut(expression as usize)?;
        specialize_typed_expression(expression, bindings, runtime);
        specialize_dependency_substitutions(expression, runtime);
    }
    Some(())
}

/// A return an inline expansion leaves through: the `return` node, its value, and whether a nested
/// lambda's template holds it.
#[derive(Clone, Copy)]
struct InlineReturn {
    returned: ExprId,
    value: Option<ExprId>,
    nested: bool,
}

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
    /// The call omits this argument: the declaration's checked default initializes the local,
    /// after every supplied argument, over the parameters before it.
    Default,
}

impl InlineOperand {
    fn receiver(name: String, local_role: IrInlineLocalRole) -> Self {
        Self {
            source_name: Some(name),
            local_role,
            role: InlineOperandRole::Receiver,
        }
    }

    fn is_spliced(&self) -> bool {
        matches!(self.role, InlineOperandRole::Spliced)
    }
}

/// The plan for one supplied argument, once its declaration mode is known.
#[derive(Debug, PartialEq, Eq)]
enum SuppliedOperandPlan {
    Splice,
    Reuse,
    Copy,
}

/// `None` when this operand has no published mode. Callers decline before allocating.
fn supplied_operand_plan(
    mode: Option<crate::types::InlineExpansionMode>,
    argument_is_local: bool,
    argument_is_lambda: bool,
) -> Option<SuppliedOperandPlan> {
    Some(match mode? {
        crate::types::InlineExpansionMode::Splice if argument_is_lambda => {
            SuppliedOperandPlan::Splice
        }
        crate::types::InlineExpansionMode::Splice if argument_is_local => {
            SuppliedOperandPlan::Reuse
        }
        crate::types::InlineExpansionMode::Splice
        | crate::types::InlineExpansionMode::Materialize
        | crate::types::InlineExpansionMode::Receiver => SuppliedOperandPlan::Copy,
    })
}

/// Build the inline expansion's semantic bindings from the checker's recorded substitutions.
///
/// Declaration headers are intentionally not accepted here. Whether a substitution may drive a
/// reified operation was decided while checking the call and travels on that exact substitution;
/// common lowering only resolves the stable module parameter identity to its template name.
fn checked_substitution_bindings<'a>(
    substitutions: &[FirTypeSubstitution],
    mut module_parameter_name: impl FnMut(crate::fir::TypeParameterId) -> Option<&'a str>,
) -> (HashMap<String, Ty>, HashMap<String, Ty>) {
    let bindings = substitutions
        .iter()
        .filter_map(|substitution| match substitution.parameter {
            FirTypeParameterRef::Module(parameter) => module_parameter_name(parameter)
                .map(|name| (name.to_owned(), substitution.value.get())),
            FirTypeParameterRef::External { .. } => None,
        })
        .collect::<HashMap<_, _>>();
    let reified_bindings = substitutions
        .iter()
        .filter(|substitution| substitution.reified)
        .filter_map(|substitution| match substitution.parameter {
            FirTypeParameterRef::Module(parameter) => module_parameter_name(parameter)
                .map(|name| (name.to_owned(), substitution.value.get())),
            FirTypeParameterRef::External { .. } => None,
        })
        .collect::<HashMap<_, _>>();
    (bindings, reified_bindings)
}

impl BodyLowering<'_> {
    /// Declare an omitted parameter's local, initialized by a copy of the declaration's checked
    /// default. The copy reads the expansion's preceding parameter locals and moves its own locals
    /// above the current temporaries. The checker numbers only parameters in scope before this
    /// default; including the parameter being initialized would reinterpret the default's first
    /// local as that parameter. A default that reads a spliced lambda parameter has no value to
    /// read.
    fn inline_default_declaration(
        &mut self,
        default: ExprId,
        operand_slots: &[Option<u32>],
        slot: u32,
        ty: Ty,
        (bindings, reified_bindings): (&HashMap<String, Ty>, &HashMap<String, Ty>),
    ) -> Option<(ExprId, HashMap<ExprId, ExprId>)> {
        const SPLICED: u32 = u32::MAX;
        let formal_slots = operand_slots
            .iter()
            .map(|slot| slot.unwrap_or(SPLICED))
            .collect::<Vec<_>>();
        let (copy, cloned) = crate::ir::clone_expression_dag(self.ir, default);
        let mut copied_expressions = cloned.values().copied().collect::<Vec<_>>();
        copied_expressions.sort_unstable();
        for copied in copied_expressions {
            specialize_inline_copy(self.ir, copied, bindings, reified_bindings)?;
        }
        let locals = super::source_calls::rehome_inline_body_values(
            self.ir,
            copy,
            &formal_slots,
            self.next_temporary,
        )?;
        self.next_temporary = self.next_temporary.checked_add(locals)?;
        if cloned
            .values()
            .any(|copied| value_indices(self.ir.expr(*copied)).contains(&SPLICED))
        {
            return None;
        }
        let declaration = self.ir.add_expr(IrExpr::Variable {
            index: slot,
            ty: stored_value_ty(ty),
            init: Some(copy),
            named: true,
        });
        self.ir.call_operand_bindings.insert(declaration);
        Some((declaration, cloned))
    }

    /// Copy an omitted parameter's default lambda for splicing. Its captures read the expansion's
    /// preceding parameter locals; its body keeps its own numbering.
    fn inline_default_lambda(
        &mut self,
        default: ExprId,
        operand_slots: &[Option<u32>],
        (bindings, reified_bindings): (&HashMap<String, Ty>, &HashMap<String, Ty>),
    ) -> Option<ExprId> {
        const SPLICED: u32 = u32::MAX;
        let formal_slots = operand_slots
            .iter()
            .map(|slot| slot.unwrap_or(SPLICED))
            .collect::<Vec<_>>();
        let (copy, cloned) = crate::ir::clone_expression_dag(self.ir, default);
        let mut copied_expressions = cloned.values().copied().collect::<Vec<_>>();
        copied_expressions.sort_unstable();
        for copied in copied_expressions {
            specialize_inline_copy(self.ir, copied, bindings, reified_bindings)?;
        }
        let IrExpr::Lambda { captures, .. } = self.ir.expr(copy).clone() else {
            return None;
        };
        for capture in captures {
            if super::source_calls::rehome_inline_body_values(self.ir, capture, &formal_slots, 0)?
                != 0
            {
                return None;
            }
            if value_indices(self.ir.expr(capture)).contains(&SPLICED) {
                return None;
            }
        }
        Some(copy)
    }

    pub(super) fn inline_same_file_call(
        &mut self,
        target: CallableId,
        function: crate::ir::FunId,
        operands: &[Option<ExprId>],
        inline_lambdas: &[Option<ExprId>],
        substitutions: &[FirTypeSubstitution],
        malformed_anonymous: &mut Option<super::FirLoweringFailure>,
    ) -> Option<ExprId> {
        let template = self.ir.functions.get(function as usize)?.body?;
        let close_line = self.ir.fn_close_lines.get(&function).copied();
        // The name its inline frames are opened under.
        let callee = self.index.callable_name(target)?.to_owned();
        let source_owner = self
            .index
            .callable(target)
            .and_then(|callable| self.index.enclosing_classifier(callable.declaration))
            .map(|classifier| classifier.classifier);
        let caller_source_name = match self.body.debug_name() {
            Some(name) => name.to_owned(),
            None => match self.expansion_enclosure {
                // These semantic scopes deliberately have no source callable spelling. A backend
                // decides how an empty caller segment participates in its physical artifact name.
                None
                | Some(
                    crate::ir::IrEnclosure::Constructor { .. }
                    | crate::ir::IrEnclosure::File
                    | crate::ir::IrEnclosure::ClassInitializer(_)
                    | crate::ir::IrEnclosure::Classifier(_)
                    | crate::ir::IrEnclosure::PropertyAccessor { .. },
                ) => String::new(),
                // Function and lambda bodies always publish their source spelling. Decline the
                // expansion if that checked provenance is absent instead of inventing one.
                Some(crate::ir::IrEnclosure::Function(_) | crate::ir::IrEnclosure::Lambda(_)) => {
                    return None
                }
            },
        };
        let caller_declaration = crate::fir::DeclarationId::from_raw(self.body.owner().raw());
        let function_shape = self.ir.functions.get(function as usize)?;
        let parameter_count = u32::try_from(
            function_shape.params.len() + usize::from(function_shape.dispatch_receiver.is_some()),
        )
        .ok()?;
        if operands.len() != parameter_count as usize || inline_lambdas.len() != operands.len() {
            return None;
        }
        let (bindings, reified_bindings) =
            checked_substitution_bindings(substitutions, |parameter| {
                self.index.type_parameter_semantic_name(parameter)
            });
        // Only a reified parameter's argument is known inside the expanded body at run time. A
        // dependency's reified operation that mentions an ordinary parameter of this declaration
        // keeps that parameter, exactly as the declaration's own emitted body would: `typeOf`
        // describes it as a type parameter rather than as whatever this call site happened to
        // infer for it.
        // The declared parameter type, before this call's substitutions. A type parameter
        // specialized to a function type is still not an inline lambda parameter.
        let declared_operand_types = function_shape
            .dispatch_receiver
            .map(Ty::obj_name)
            .into_iter()
            .chain(function_shape.params.iter().copied())
            .collect::<Vec<_>>();
        let operand_types = declared_operand_types
            .iter()
            .copied()
            .map(|ty| ty_subst_keep_unbound(ty, &bindings))
            .collect::<Vec<_>>();
        // The declaration published one mode per semantic parameter. A missing or unaligned mode
        // declines before any operand is copied into the arena: guessing splice or copy would
        // change identity and evaluation.
        let modes = {
            let stored = self.ir.inline_expansion_modes(function)?;
            let mut modes = Vec::with_capacity(operands.len());
            if self
                .ir
                .functions
                .get(function as usize)?
                .dispatch_receiver
                .is_some()
            {
                modes.push(crate::types::InlineExpansionMode::Receiver);
            }
            modes.extend_from_slice(stored);
            if modes.len() != operands.len() {
                return None;
            }
            modes
        };
        // Resolve every operand's published mode before copying arguments. A receiver mode on a
        // value parameter, or a mode list that does not cover the identities, declines here.
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
            let mut mode_at = parameter_names.len();
            for identity in identities {
                let mode = modes.get(mode_at).copied()?;
                mode_at += 1;
                if matches!(identity.role, crate::ir::IrParameterRole::ExtensionReceiver) {
                    if mode != crate::types::InlineExpansionMode::Receiver {
                        return None;
                    }
                    parameter_names.push(InlineOperand::receiver(
                        self.ir.functions[function as usize].name.clone(),
                        IrInlineLocalRole::ExtensionReceiver,
                    ));
                    continue;
                }
                parameter_names.push(InlineOperand {
                    source_name: identity.source_name.clone(),
                    local_role: IrInlineLocalRole::Value,
                    role: match mode {
                        crate::types::InlineExpansionMode::Splice => InlineOperandRole::Spliced,
                        crate::types::InlineExpansionMode::Materialize => {
                            InlineOperandRole::Materialized
                        }
                        crate::types::InlineExpansionMode::Receiver => return None,
                    },
                });
            }
            if mode_at != modes.len() {
                return None;
            }
        } else if !modes.is_empty() {
            return None;
        }
        let operands = operands
            .iter()
            .map(|operand| {
                operand.map(|operand| self.specialized_inline_operand(operand, &bindings))
            })
            .collect::<Vec<_>>();
        let read = crate::ir::read_values(self.ir, template);
        for (value, operand) in operands.iter().enumerate() {
            if let Some(operand) = *operand {
                if !read.contains(&(value as u32)) {
                    self.ir.mark_unread_inline_operand(operand);
                }
            }
        }
        let defaults = if operands.iter().any(Option::is_none) {
            if self.ir.param_defaults_stub_only(function) {
                return None;
            }
            // The declaration's defaults are numbered like its parameters, without the dispatch
            // receiver the operands lead with.
            let receiver_offset =
                operands.len() - self.ir.functions[function as usize].params.len();
            let declared = self.ir.param_defaults(function)?;
            operands
                .iter()
                .enumerate()
                .map(|(index, operand)| match operand {
                    Some(_) => Some(None),
                    None => declared
                        .get(index.checked_sub(receiver_offset)?)
                        .copied()
                        .flatten()
                        .map(Some),
                })
                .collect::<Option<Vec<_>>>()?
        } else {
            vec![None; operands.len()]
        };
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
        // Which parameter splices was recorded with its identity: a non-null function type that
        // did not write `noinline` splices, and a receiver, `noinline`, nullable function, type
        // parameter, or `Any` materializes. The argument's type is not consulted again.
        //
        // A parameter with no name or no published role to align against is a broken contract
        // between this expansion and the callable's published header, not a shape to fall back on:
        // either answer silently erases something — the parameter's identity, or the splice.
        //
        // An omitted function-typed parameter whose default is a lambda literal is expanded like
        // a literal argument, as kotlinc inlines a default lambda; any other omitted parameter is
        // a local its default initializes.
        let default_lambdas = defaults
            .iter()
            .enumerate()
            .filter(|&(index, default)| {
                default.is_some_and(|default| {
                    matches!(
                        self.ir.expr(default),
                        IrExpr::Lambda {
                            inline_body: Some(_),
                            ..
                        }
                    )
                }) && parameter_names
                    .get(index)
                    .is_some_and(InlineOperand::is_spliced)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let mut plans = Vec::with_capacity(operands.len());
        let mut splice_lambda = vec![false; operands.len()];
        for (index, (operand, lambda)) in operands.iter().zip(inline_lambdas).enumerate() {
            let Some(operand) = operand else {
                let splice = default_lambdas.contains(&index);
                splice_lambda[index] = splice;
                plans.push(if splice {
                    InlineOperandPlan::Splice
                } else {
                    InlineOperandPlan::Default
                });
                continue;
            };
            parameter_names.get(index)?;
            let plan = match supplied_operand_plan(
                modes.get(index).copied(),
                matches!(self.ir.expr(*operand), IrExpr::GetValue(_)),
                lambda.is_some(),
            ) {
                Some(SuppliedOperandPlan::Splice) => InlineOperandPlan::Splice,
                Some(SuppliedOperandPlan::Reuse) => {
                    let IrExpr::GetValue(slot) = self.ir.expr(*operand) else {
                        return None;
                    };
                    InlineOperandPlan::Reuse(*slot)
                }
                Some(SuppliedOperandPlan::Copy) => InlineOperandPlan::Copy,
                None => return None,
            };
            splice_lambda[index] = matches!(plan, InlineOperandPlan::Splice);
            plans.push(plan);
        }
        let mut operand_declarations = Vec::new();
        let mut defaulted = Vec::new();
        let operand_slots = plans
            .into_iter()
            .zip(operands.iter())
            .zip(operand_types.iter().zip(&declared_operand_types))
            .enumerate()
            .map(
                |(index, ((plan, operand), (specialized_ty, declared_ty)))| match plan {
                    InlineOperandPlan::Splice => None,
                    InlineOperandPlan::Reuse(slot) => Some(slot),
                    InlineOperandPlan::Default => {
                        let slot = self.allocate_temporary();
                        defaulted.push((index, slot));
                        Some(slot)
                    }
                    InlineOperandPlan::Copy => {
                        let operand = operand.expect("a copied operand is supplied");
                        let slot = self.allocate_temporary();
                        let declaration = self.ir.add_expr(IrExpr::Variable {
                            index: slot,
                            ty: stored_value_ty(*specialized_ty),
                            init: Some(operand),
                            named: true,
                        });
                        self.ir.call_operand_bindings.insert(declaration);
                        self.ir
                            .record_inline_operand_declared_type(declaration, *declared_ty);
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
                },
            )
            .collect::<Vec<_>>();
        let mut inline_lambdas = inline_lambdas.to_vec();
        // A copied lambda is a value. Leaving it in this table would replace every read with a
        // fresh copy of the literal, so `x === x` would compare two objects.
        for (lambda, splice) in inline_lambdas.iter_mut().zip(&splice_lambda) {
            if !splice {
                *lambda = None;
            }
        }
        let mut default_lambda_implementations = Vec::new();
        let mut default_copies = Vec::new();
        for &index in &default_lambdas {
            let lambda = self.inline_default_lambda(
                defaults[index]?,
                &operand_slots[..index],
                (&bindings, &reified_bindings),
            )?;
            let IrExpr::Lambda { impl_fn, .. } = *self.ir.expr(lambda) else {
                return None;
            };
            default_lambda_implementations.push(impl_fn);
            inline_lambdas[index] = Some(lambda);
        }
        for (index, slot) in defaulted {
            let default = defaults[index]?;
            let (declaration, cloned) = self.inline_default_declaration(
                default,
                &operand_slots[..index],
                slot,
                operand_types[index],
                (&bindings, &reified_bindings),
            )?;
            default_copies.extend(cloned);
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
        }
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
        // A materialized/noinline default lambda is not part of the declaration body template,
        // but its implementation crosses the same inline boundary. Keep its inline-body template
        // protected while making the lambda expression itself available for implementation
        // specialization below.
        for &(source, _) in &default_copies {
            if let IrExpr::Lambda {
                inline_body: Some(body),
                ..
            } = self.ir.expr(source)
            {
                mark_subtree(self.ir, *body, &mut protected);
            }
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
        let mut returns: Vec<InlineReturn> = Vec::new();
        // Copies whose `GetValue` of an inline parameter was replaced by that parameter's lambda.
        // A capture temporary initialized from one of these is the lambda, even though the
        // invocation still reads the temporary.
        let mut substituted_inline_lambdas = HashSet::new();

        // Clone order, not map order: rewriting the copies allocates locals and records returns,
        // and both orders reach the emitted code.
        let mut copies = cloned
            .iter()
            .map(|(&source, &copy)| (source, copy))
            .collect::<Vec<_>>();
        copies.sort_by_key(|&(_, copy)| copy);
        for &(source, copy) in &copies {
            self.ir.record_inline_copy_owner(copy, source_owner);
            // A compiler temporary's synthetic zero is refreshed after its type specializes.
            // A deferred source local carries explicit declaration provenance instead: its
            // semantic type specializes normally, and each backend selects its physical zero.
            let generated_zero = synthetic_temporary_default(self.ir, source);
            // Reified/type-parameter substitutions are lexical: they apply inside nested lambda
            // templates even though those templates own an independent value-numbering domain.
            // Value rebasing and return rewriting remain protected below, but the checked type
            // decision must cross the enclosing inline-call boundary with the lambda.
            specialize_inline_copy(self.ir, copy, &bindings, &reified_bindings)?;
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
                // A nested lambda's template keeps its own numbering, but a return it carries that
                // its boundaries already moved to this declaration leaves this expansion too.
                if let (IrExpr::Return(value), Some(0)) = (
                    self.ir.expr(copy).clone(),
                    self.ir.checked_return_depths.get(&copy).copied(),
                ) {
                    returns.push(InlineReturn {
                        returned: copy,
                        value,
                        nested: true,
                    });
                    self.ir.checked_return_depths.remove(&copy);
                }
                continue;
            }
            if let IrExpr::GetValue(parameter) = self.ir.expr(source) {
                if let Some(Some(lambda)) = inline_lambdas.get(*parameter as usize) {
                    self.ir.exprs[copy as usize] = self.ir.expr(*lambda).clone();
                    substituted_inline_lambdas.insert(copy);
                    self.ir.unmark_inline_copy(copy);
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
                returns.push(InlineReturn {
                    returned: copy,
                    value,
                    nested: false,
                });
                // This return has crossed its checked callable boundary and will be represented by
                // the block that replaces it, in EITHER shape. The sparse depth fact belongs to the
                // old `Return` node; leaving it on the replacement makes an enclosing inline-lambda
                // template try to consume the same return a second time. Removing it is a fact
                // about the node, not a commitment to the loop shape, so it happens here.
                self.ir.checked_return_depths.remove(&copy);
            }
        }
        // An escaping lambda keeps its own implementation method. Specializing only the inline
        // template would leave `as? T` erased on that method, so the call site still runs the
        // declaration's check. Clone the implementation when this expansion fixes a type it uses.
        let mut escaping_copies = copies.clone();
        escaping_copies.extend(default_copies);
        escaping_lambda::specialize(
            self.ir,
            &escaping_copies,
            &protected,
            &bindings,
            &reified_bindings,
            caller_declaration,
            self.expansion_enclosure,
            self.in_default_argument,
            &caller_source_name,
            target,
            &callee,
        );
        // An anonymous object's methods are not children of the inlined template. Cloning only the
        // `new` reuses the declaration class, whose reified parameter is erased.
        if let Err(failure) = escaping_anonymous::specialize(
            self.ir,
            escaping_copies
                .iter()
                .filter(|(source, _)| !protected.contains(source))
                .map(|(_, copy)| *copy),
            &bindings,
            &reified_bindings,
            &escaping_anonymous::CallSite {
                caller_declaration,
                caller: self.expansion_enclosure,
                caller_is_default: self.in_default_argument,
                caller_source_name: &caller_source_name,
                inline_callee: target,
                inline_callee_source_name: &callee,
            },
        ) {
            *malformed_anonymous = Some(failure);
            return None;
        }

        // A function-value conversion between an inline parameter and its lambda is a
        // callable reference stored in a local. The invocation reads that local, so the
        // splice below never sees the lambda. Retarget the invocation when that local is
        // only invoked; the carrier is then not evaluated.
        let converted_unit_results =
            self.expose_inline_lambdas_behind_function_value_conversions(&copies);
        // `run { x(i) }` copies the inline lambda into an unnamed temporary and invokes that
        // temporary. Retarget the invocation while the substituted lambda is still the
        // temporary's initializer, before the direct-lambda splice below.
        self.expose_inline_lambdas_behind_capture_copies(&copies, &substituted_inline_lambdas);

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
        // A default lambda's implementation stays a method of the declaration's default stub,
        // which the expansion's splice must not consume.
        let default_lambda_methods = default_lambda_implementations
            .iter()
            .map(|&function| {
                (
                    function,
                    self.ir
                        .functions
                        .get(function as usize)
                        .and_then(|f| f.body),
                    self.ir.inline_only_fns.contains(&function),
                )
            })
            .collect::<Vec<_>>();
        for invocation in inline_invocations {
            self.splice_inline_lambda(
                invocation,
                LambdaParameterBinding::Declared {
                    callee: Some(&callee),
                },
            )?;
        }
        for invocation in converted_unit_results {
            self.discard_converted_lambda_result(invocation);
        }
        for (function, body, inline_only) in default_lambda_methods {
            self.ir.functions.get_mut(function as usize)?.body = body;
            if !inline_only {
                self.ir.inline_only_fns.remove(&function);
            }
        }

        // kotlinc opens the expansion's frame once its operands are bound.
        operand_declarations
            .push(self.inline_marker(callee, IrDebugLocalProvenance::FunctionFrameMarker));

        // An expansion whose ONLY return is its tail needs neither a result local nor the loop that
        // carries a non-local return out: the value is simply the body's value, which is what kotlinc
        // emits — it leaves it on the operand stack. The loop form costs an unnamed local, and when
        // the expansion crosses a suspension that local takes a continuation field kotlinc has no
        // counterpart for.
        if let [InlineReturn {
            returned: tail,
            value,
            nested: false,
        }] = returns[..]
        {
            if produce_sole_tail_return(self.ir, cloned_root, tail, value, close_line) {
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
        for found in returns {
            self.ir.exprs[found.returned as usize] = super::inline_returns::frame_exit(
                self.ir,
                &label,
                result_slot,
                found.nested,
                found.value,
            );
        }

        let mut statements = operand_declarations;
        if let Some(slot) = result_slot {
            let initial = self
                .ir
                .add_expr(IrExpr::Const(IrConst::zero_for_value_type(result_ty)));
            let declaration = self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty: stored_value_ty(result_ty),
                init: Some(initial),
                named: false,
            });
            self.ir
                .inline_return_frames
                .insert(declaration, label.clone());
            statements.push(declaration);
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
        let value = match result_slot {
            None => {
                let unit = self.ir.add_expr(IrExpr::UnitInstance);
                self.ir.record_inline_copy_owner(unit, source_owner);
                if let Some(line) = close_line {
                    self.ir.expr_source_lines.insert(unit, line);
                }
                self.ir.retain_inline_unit_line(unit);
                Some(unit)
            }
            Some(slot) => Some(self.ir.add_expr(IrExpr::GetValue(slot))),
        };
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

    /// Record an inline-depth frame boundary named `callee`, live to the end of the block that
    /// declares it. It is not a value: the JVM debug boundary materializes the slot, the zero
    /// store, and the spelling.
    pub(super) fn inline_marker(
        &mut self,
        callee: String,
        provenance: IrDebugLocalProvenance,
    ) -> ExprId {
        let declaration = self.ir.add_expr(IrExpr::InlineFrameMarker);
        self.ir.value_names.insert(declaration, callee);
        self.ir.set_debug_local_provenance(declaration, provenance);
        declaration
    }

    pub(super) fn splice_inline_lambda(
        &mut self,
        invocation: ExprId,
        binding: LambdaParameterBinding<'_>,
    ) -> Option<()> {
        let IrExpr::InvokeFunction {
            func,
            args,
            params: _,
            ret: _,
        } = self.ir.expr(invocation).clone()
        else {
            return None;
        };
        if !lambda_invocation_is_spliceable(self.ir, invocation, func) {
            return None;
        }
        let IrExpr::Lambda {
            impl_fn,
            captures,
            inline_body: Some(inline_body),
            ..
        } = self.ir.expr(func).clone()
        else {
            return None;
        };
        let parameter_types = self.ir.functions.get(impl_fn as usize)?.params.clone();

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
                match (self.ir.expr(value), &source_name, binding) {
                    (IrExpr::GetValue(slot), None, _)
                    | (IrExpr::GetValue(slot), _, LambdaParameterBinding::Remapped) => *slot,
                    (_, _, LambdaParameterBinding::Remapped) => return None,
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
        // Once its parameters are bound, the body opens its own frame, named after the inline
        // callable it was passed to.
        if let LambdaParameterBinding::Declared {
            callee: Some(callee),
        } = binding
        {
            declarations.push(self.inline_marker(
                callee.to_owned(),
                IrDebugLocalProvenance::LambdaFrameMarker {
                    implementation: impl_fn,
                    depth: 0,
                },
            ));
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
        // The spliced body's own calls are the suspension points now, not the invocation.
        self.ir.suspend_calls.remove(&invocation);
        self.ir.suspend_call_overridden_results.remove(&invocation);
        Some(())
    }
}

/// Whether `invocation` and its inline `lambda` have the parameter shape the splicer consumes.
/// A caller that exposes a lambda hidden behind a semantic wrapper uses this same gate before
/// retargeting the invocation, so the preparatory rewrite and the splice cannot drift apart.
fn lambda_invocation_is_spliceable(
    ir: &crate::ir::IrFile,
    invocation: ExprId,
    lambda: ExprId,
) -> bool {
    let IrExpr::InvokeFunction { args, .. } = ir.expr(invocation) else {
        return false;
    };
    let IrExpr::Lambda {
        impl_fn,
        arity,
        captures,
        inline_body: Some(_),
        ..
    } = ir.expr(lambda)
    else {
        return false;
    };
    // A suspend lambda's arity counts the continuation its invocation passes implicitly.
    let suspend = ir.suspend_funs.contains(impl_fn);
    args.len() + usize::from(suspend) == *arity as usize
        && ir
            .functions
            .get(*impl_fn as usize)
            .is_some_and(|function| function.params.len() == captures.len() + args.len())
}

/// How a spliced lambda's parameters meet the arguments of its invocation.
#[derive(Clone, Copy)]
pub(super) enum LambdaParameterBinding<'callee> {
    /// A named parameter becomes a local of the splice holding its argument, as at an inline
    /// function's call site. `callee` names the inline callable whose frame the body opens inside,
    /// when the splice realizes one: the body then declares that frame's inline-depth marker.
    Declared { callee: Option<&'callee str> },
    /// Every argument is a read of a value the parameter simply becomes, with no local of its own:
    /// kotlinc's `IrInlinable.inline`, which remaps the lambda's parameters onto the variables it
    /// is given. An argument that is not such a read cannot be spliced this way.
    Remapped,
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

/// The synthetic zero of a compiler temporary. Deferred source locals carry an explicit
/// declaration fact in common IR instead of being recognized from their constant's shape.
fn synthetic_temporary_default(ir: &crate::ir::IrFile, source: ExprId) -> Option<IrConst> {
    let IrExpr::Variable {
        ty,
        init: Some(initial),
        named,
        ..
    } = ir.expr(source)
    else {
        return None;
    };
    if *named {
        return None;
    }
    let IrExpr::Const(value) = ir.expr(*initial) else {
        return None;
    };
    if *value != IrConst::zero_for_value_type(*ty) {
        return None;
    }
    Some(value.clone())
}

/// `bindings` substitutes static signatures. `runtime` substitutes operations that execute at run
/// time (`is`, `as`, `as?`, `typeOf`, `T::class`). An ordinary type parameter is in `bindings` and
/// not in `runtime`, so a lambda copied for a reified sibling does not reify the erased parameter.
pub(super) fn specialize_typed_expression(
    expression: &mut IrExpr,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
) {
    match expression {
        IrExpr::Checked(operation) => specialize_checked_operation(operation, bindings),
        IrExpr::CallableReference(reference) => {
            if let crate::ir::IrCallableReferenceTarget::External {
                receiver: Some(receiver),
                ..
            } = &mut reference.target
            {
                specialize_ty(receiver, bindings);
            }
            specialize_ty(&mut reference.function_type, bindings);
            specialize_tys(&mut reference.declaration_parameters, bindings);
            specialize_ty(&mut reference.declaration_result, bindings);
            specialize_reference_adaptation(&mut reference.adaptation, bindings);
        }
        IrExpr::KClassLiteral { classifier, .. } => specialize_optional_ty(classifier, runtime),
        IrExpr::LocalPropertyReference(reference) => {
            specialize_ty(&mut reference.property_type, bindings)
        }
        IrExpr::LocalDelegateAccess(_) => {}
        IrExpr::Call { callee, .. } => specialize_callee(callee, bindings, runtime),
        IrExpr::TypeOp {
            op, type_operand, ..
        } => {
            let declaration_generic_target = matches!(type_operand.non_null(), Ty::TyParam(..));
            specialize_ty(type_operand, runtime);
            // `as T` is checked against T's declaration bound, so an unconstrained T initially
            // admits null. Once an inline call fixes a reified T to a concrete non-null type, the
            // checked operation has non-null cast semantics at that use site. An ordinary type
            // parameter stays a type parameter: the cast remains erased.
            if declaration_generic_target
                && *op == crate::ir::IrTypeOp::Cast
                && !matches!(type_operand.non_null(), Ty::TyParam(..))
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
        } => specialize_ty(type_operand, bindings),
        IrExpr::Try {
            result: type_operand,
            catches,
            ..
        } => {
            specialize_ty(type_operand, bindings);
            // The semantic type is the only catch fact. A reified parameter stays a parameter
            // until this substitution; emission derives the JVM class from whatever `ty` is then.
            for catch in catches {
                specialize_ty(&mut catch.ty, runtime);
            }
        }
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
        | IrExpr::ForwardedSuperArgument { .. }
        | IrExpr::SetValue { .. }
        | IrExpr::SetFrameResult { .. }
        | IrExpr::Return(_)
        | IrExpr::Block { .. }
        | IrExpr::When { .. }
        | IrExpr::While { .. }
        | IrExpr::Break { .. }
        | IrExpr::Continue { .. }
        | IrExpr::PrimitiveBinOp { .. }
        | IrExpr::Equality { .. }
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
        | IrExpr::InlineFrameMarker
        | IrExpr::NotNullAssert { .. }
        | IrExpr::LateinitCheck { .. }
        | IrExpr::ExternalStaticInstance { .. }
        | IrExpr::Throw { .. } => {}
    }
}

pub(super) fn specialize_recorded_facts(
    ir: &mut crate::ir::IrFile,
    expression: ExprId,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
) {
    for ty in [
        ir.logical_types.get_mut(&expression),
        ir.whens.exhaustive.get_mut(&expression),
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
    if let Some(access) = ir.module_member_accesses.get_mut(&expression) {
        let parameters = match access {
            crate::ir::IrModuleMemberAccess::Callable {
                selected_parameters,
                ..
            }
            | crate::ir::IrModuleMemberAccess::Property {
                selected_parameters,
                ..
            } => selected_parameters,
        };
        specialize_tys(parameters, bindings);
    }
    if let Some(parameters) = ir.construction_declared_params.get_mut(&expression) {
        specialize_tys(parameters, bindings);
    }
    if let Some(substitutions) = ir.reified_call_subst.get_mut(&expression) {
        for (_, ty) in substitutions {
            specialize_ty(ty, runtime);
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
            | IrValueClassSuspendResult::Carrier { carrier, .. } => {
                specialize_ty(carrier, bindings)
            }
        }
    }
    if let Some(point) = ir.intrinsic_suspension_points.get_mut(&expression) {
        specialize_ty(&mut point.result, bindings);
    }
}

fn specialize_ty(ty: &mut Ty, bindings: &HashMap<String, Ty>) {
    *ty = ty_subst_keep_unbound(*ty, bindings).canonical_semantic();
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
    *ty = ResolvedTy::new(ty_subst_keep_unbound(ty.get(), bindings).canonical_semantic())
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
        | IrIntrinsic::EnumEntries {
            classifier: operand,
        }
        | IrIntrinsic::GeneratedPropertyEquals { ty: operand }
        | IrIntrinsic::Ieee754Equals { operand }
        | IrIntrinsic::GeneratedPropertyHash { ty: operand }
        | IrIntrinsic::DataClassArrayToString { ty: operand }
        | IrIntrinsic::TypeOf { ty: operand } => specialize_ty(operand, bindings),
        IrIntrinsic::Assert { .. }
        | IrIntrinsic::ArrayGet
        | IrIntrinsic::ArraySet
        | IrIntrinsic::ArraySize
        | IrIntrinsic::StringGet
        | IrIntrinsic::StringLength
        | IrIntrinsic::EnumName
        | IrIntrinsic::NullableAnyToString
        | IrIntrinsic::CoroutineContext => {}
    }
}

fn specialize_callee(
    callee: &mut Callee,
    bindings: &HashMap<String, Ty>,
    runtime: &HashMap<String, Ty>,
) {
    match callee {
        Callee::Intrinsic { operation, ret } => {
            specialize_intrinsic(operation, runtime);
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
pub(super) fn specialize_dependency_substitutions(
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
            getter_inline_splice,
            ..
        } => {
            if let Some(receiver) = receiver {
                specialize_resolved_ty(receiver, bindings);
            }
            specialize_resolved_ty(property_type, bindings);
            if let Some(splice) = getter_inline_splice {
                for substitution in &mut splice.substitutions {
                    specialize_resolved_ty(&mut substitution.value, bindings);
                }
            }
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
        IrCheckedOperation::IllegalProgressionStep { .. } => {}
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
    close_line: Option<u32>,
) -> bool {
    let Some(chain) = tail_statement_block_chain(ir, root, tail) else {
        return false;
    };
    let produced = value.unwrap_or_else(|| ir.add_expr(IrExpr::UnitInstance));
    if matches!(ir.expr(produced), IrExpr::UnitInstance) {
        ir.copy_inline_copy_mark(tail, produced);
        if !ir.expr_source_lines.contains_key(&produced) {
            if let Some(line) = ir
                .expr_source_lines
                .get(&tail)
                .copied()
                .or_else(|| ir.fallthrough_return_line(tail))
                .or_else(|| ir.expr_end_lines.get(&tail).copied())
                .or_else(|| ir.expr_end_lines.get(&root).copied())
                .or(close_line)
            {
                ir.expr_source_lines.insert(produced, line);
            }
        }
        ir.retain_inline_unit_line(produced);
    }
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
mod expansion_mode_tests {
    use super::{checked_substitution_bindings, supplied_operand_plan};
    use crate::fir::{FirTypeParameterRef, FirTypeSubstitution, ResolvedTy, TypeParameterId};
    use crate::types::{InlineExpansionMode, Ty};

    #[test]
    fn checked_substitution_flag_alone_selects_reified_bindings() {
        let ordinary = TypeParameterId::from_raw(3);
        let reified = TypeParameterId::from_raw(7);
        let substitutions = [
            FirTypeSubstitution {
                parameter: FirTypeParameterRef::Module(ordinary),
                reified: false,
                value: ResolvedTy::new(Ty::String).unwrap(),
                additional_bounds: Box::new([]),
            },
            FirTypeSubstitution {
                parameter: FirTypeParameterRef::Module(reified),
                reified: true,
                value: ResolvedTy::new(Ty::Int).unwrap(),
                additional_bounds: Box::new([]),
            },
        ];

        let (bindings, reified_bindings) =
            checked_substitution_bindings(&substitutions, |parameter| {
                if parameter == ordinary {
                    Some("T")
                } else if parameter == reified {
                    Some("R")
                } else {
                    None
                }
            });

        assert_eq!(
            bindings,
            [("T".into(), Ty::String), ("R".into(), Ty::Int)].into()
        );
        assert_eq!(reified_bindings, [("R".into(), Ty::Int)].into());
    }

    #[test]
    fn a_missing_mode_declines_a_lambda_instead_of_copying_it() {
        assert!(supplied_operand_plan(None, false, true).is_none());
        assert!(supplied_operand_plan(None, true, false).is_none());
    }

    #[test]
    fn a_published_mode_chooses_splice_reuse_or_copy() {
        use super::SuppliedOperandPlan;
        assert_eq!(
            supplied_operand_plan(Some(InlineExpansionMode::Splice), false, true),
            Some(SuppliedOperandPlan::Splice)
        );
        assert_eq!(
            supplied_operand_plan(Some(InlineExpansionMode::Splice), true, false),
            Some(SuppliedOperandPlan::Reuse)
        );
        assert_eq!(
            supplied_operand_plan(Some(InlineExpansionMode::Materialize), false, true),
            Some(SuppliedOperandPlan::Copy)
        );
        assert_eq!(
            supplied_operand_plan(Some(InlineExpansionMode::Receiver), false, true),
            Some(SuppliedOperandPlan::Copy)
        );
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
            source: crate::ir::IrProgressionSource::Literal {
                operation: FirRangeOperation::Until,
                start: 0,
                end: 1,
            },
            unsigned_compare: None,
            body: 2,
            label: "loop".to_string(),
            with_index: None,
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
            Some(returned),
            None,
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
            Some(returned),
            None,
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
            Some(returned),
            None,
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

        assert!(!produce_sole_tail_return(&mut ir, block, tail, None, None,));
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
            Some(returned),
            None,
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
            Some(returned),
            None,
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
