//! Call-site expansion of inline bodies into expression-local frames: the retained checked bodies
//! of same-module inline functions, and the templates of inline dependency functions
//! ([`InlineCallee`]).

use crate::fx_hash::{HashMap, HashSet};

use crate::fir::FirTypeSubstitution;
use crate::ir::{ExprId, IrConst, IrDebugLocalProvenance, IrExpr, IrInlineLocalRole};
use crate::types::{stored_value_ty, ty_subst_keep_unbound, Ty};

use super::BodyLowering;
use crate::inline_expansion::{
    mark_subtree, produce_sole_tail_return, rebase_values, specialize_inline_copy, InlineReturn,
};

mod caller_sites;
mod converted_inline_lambda;
mod escaping_anonymous;
mod escaping_lambda;
mod immediate_lambda;
mod inline_callee;
mod lambda_splice_contract;
mod property_accessors;

pub(super) use immediate_lambda::splice_immediate_lambda;
pub(super) use inline_callee::InlineCallee;
pub(super) use lambda_splice_contract::LambdaParameterBinding;

#[cfg(test)]
mod escaping_inline_object;
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

/// Direct lambda values whose invocation the declared-lambda splicer will consume. This query is
/// intentionally mutation-free so escaping specialization can exclude those non-escaping values
/// before any later exposure rewrite commits.
fn directly_spliceable_invocations(
    ir: &crate::ir::IrFile,
    copies: &[(ExprId, ExprId)],
) -> Vec<ExprId> {
    copies
        .iter()
        .map(|&(_, copy)| copy)
        .filter(|expression| {
            matches!(
                ir.expr(*expression),
                IrExpr::InvokeFunction { func, .. }
                    if lambda_splice_contract::invocation_is_spliceable(
                        ir,
                        *expression,
                        *func,
                    )
            )
        })
        .collect()
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
        if cloned.values().any(|copied| {
            crate::inline_expansion::value_indices(self.ir.expr(*copied)).contains(&SPLICED)
        }) {
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
            if crate::inline_expansion::value_indices(self.ir.expr(capture)).contains(&SPLICED) {
                return None;
            }
        }
        Some(copy)
    }

    /// Expand a call of `inline_callee`, whose body is `function`'s in this file, over the call's
    /// operands.
    pub(super) fn inline_same_file_call(
        &mut self,
        inline_callee: &InlineCallee,
        function: crate::ir::FunId,
        operands: &[Option<ExprId>],
        inline_lambdas: &[Option<ExprId>],
        substitutions: &[FirTypeSubstitution],
        malformed_anonymous: &mut Option<super::FirLoweringFailure>,
    ) -> Option<ExprId> {
        let template = self.ir.functions.get(function as usize)?.body?;
        let close_line = self.ir.function_position(function).close_line;
        let frame = inline_callee.frame(self.index)?;
        // The name its inline frames are opened under.
        let callee = frame.name.clone();
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
                Some(crate::ir::IrEnclosure::Lambda(_)) if self.allows_empty_inline_caller_name => {
                    String::new()
                }
                // A named function, or a lambda without a checked nameless root context, must
                // retain its source spelling. Never invent one when that provenance is absent.
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
        let index = self.index;
        let (bindings, reified_bindings) = inline_callee
            .substitution_bindings(substitutions, |parameter| {
                index.type_parameter_semantic_name(parameter)
            })?;
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
        let mut staging_failed = false;
        let operand_slots = plans
            .into_iter()
            .zip(operands.iter())
            .zip(operand_types.iter().zip(&declared_operand_types))
            .enumerate()
            .map(
                |(index, ((plan, operand), (specialized_ty, declared_ty)))| match plan {
                    InlineOperandPlan::Splice => {
                        // A spliced reference is not a value, but its receiver and captures are
                        // ordinary argument expressions: they run here, before the next argument,
                        // and the body reads the values.
                        if let Some(operand) = *operand {
                            if self
                                .stage_spliced_reference_operands(
                                    operand,
                                    &mut operand_declarations,
                                )
                                .is_none()
                            {
                                staging_failed = true;
                            }
                        }
                        None
                    }
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
                            .record_inline_declared_type(declaration, *declared_ty);
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
        if staging_failed {
            return None;
        }
        let mut inline_lambdas = inline_lambdas.to_vec();
        // A copied lambda is a value. Leaving it in this table would replace every read with a
        // fresh copy of the literal, so `x === x` would compare two objects.
        for (lambda, splice) in inline_lambdas.iter_mut().zip(&splice_lambda) {
            if !splice {
                *lambda = None;
            } else if let Some(reference) = *lambda {
                if matches!(self.ir.expr(reference), IrExpr::CallableReference(_)) {
                    self.ir
                        .inline_callable_reference_arguments
                        .insert(reference);
                }
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
            "inline callee={} substitutions={substitutions:?} bindings={bindings:?}",
            frame.name
        );
        let result_ty = ty_subst_keep_unbound(self.ir.functions[function as usize].ret, &bindings);

        // A lambda's `inline_body` has its own value numbering and return target. It is cloned for
        // later HOF splicing, but must not be rewritten as part of the enclosing inline function.
        let mut protected = HashSet::default();
        let mut pending = vec![template];
        let mut seen = HashSet::default();
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
            .filter_map(|source| {
                crate::inline_expansion::value_indices(self.ir.expr(*source))
                    .into_iter()
                    .max()
            })
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
        let mut substituted_inline_lambdas = HashSet::default();
        // The substituted lambdas a `crossinline` parameter received. Their bodies keep that
        // origin once spliced: kotlinc exports such a body's private access wherever it runs.
        let crossinline_operands = {
            let dispatch = usize::from(
                self.ir
                    .functions
                    .get(function as usize)?
                    .dispatch_receiver
                    .is_some(),
            );
            let modifiers = self
                .ir
                .fn_params
                .get(&function)
                .map(|info| info.inline_modifiers.clone())
                .unwrap_or_default();
            (0..operands.len())
                .map(|operand| {
                    operand.checked_sub(dispatch).is_some_and(|parameter| {
                        modifiers.get(parameter)
                            == Some(&crate::types::InlineParameterModifier::Crossinline)
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut crossinline_literals = HashSet::default();

        // Clone order, not map order: rewriting the copies allocates locals and records returns,
        // and both orders reach the emitted code.
        let mut copies = cloned
            .iter()
            .map(|(&source, &copy)| (source, copy))
            .collect::<Vec<_>>();
        copies.sort_by_key(|&(_, copy)| copy);
        for &(source, copy) in &copies {
            frame.record_copy(self.ir, copy, function);
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
                    crate::inline_expansion::leaves_inline_frame(self.ir, copy);
                    returns.push(InlineReturn {
                        returned: copy,
                        value,
                        nested: true,
                    });
                }
                continue;
            }
            if let IrExpr::GetValue(parameter) = *self.ir.expr(source) {
                if let Some(Some(lambda)) = inline_lambdas.get(parameter as usize) {
                    let lambda = *lambda;
                    self.ir.exprs[copy as usize] = self.ir.expr(lambda).clone();
                    // The parameter read's end line is the callee's `block(this)`. The splice's
                    // closing `nop` belongs on the call-site lambda's `}`.
                    if let Some(end) = self.ir.end_line(lambda) {
                        self.ir.set_end_line(copy, end);
                    }
                    // The parameter read carried the inline body's line. A callable reference is
                    // the call site's expression: the spliced invocation keeps that line, and the
                    // frame-closing nop can then return to the inlined call.
                    if matches!(self.ir.expr(lambda), IrExpr::CallableReference(_)) {
                        // A copy that an invocation splices is marked there. A copy the body
                        // evaluates (an object capturing the reference) stays a carrier.
                        match self.ir.source_line(lambda) {
                            Some(line) => {
                                self.ir.set_source_line(copy, line);
                            }
                            None => {
                                self.ir.take_source_line(copy);
                            }
                        }
                    }
                    substituted_inline_lambdas.insert(copy);
                    if crossinline_operands.get(parameter as usize) == Some(&true) {
                        crossinline_literals.insert(copy);
                    }
                    self.ir.unmark_inline_copy(copy);
                    self.ir.binding_read_stability.remove(&copy);
                    if let Some(ty) = self.ir.logical_type(lambda) {
                        self.ir.set_logical_type(copy, ty);
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
            // A return that leaves this frame is represented by the block that replaces it, in
            // EITHER shape, and its sparse depth fact is consumed with it: left on the
            // replacement, an enclosing inline-lambda template would consume the same return a
            // second time. A deeper one stays a return, one lexical frame nearer its target.
            if let Some(value) = returned {
                if crate::inline_expansion::leaves_inline_frame(self.ir, copy) {
                    returns.push(InlineReturn {
                        returned: copy,
                        value,
                        nested: false,
                    });
                }
            }
        }
        // An escaping lambda keeps its own implementation method. Specializing only the inline
        // template would leave `as? T` erased on that method, so the call site still runs the
        // declaration's check. Clone the implementation when this expansion fixes a type it uses.
        let mut escaping_copies = copies.clone();
        escaping_copies.extend(default_copies);
        let anonymous_owned_substitution_edges =
            escaping_anonymous::owned_substituted_lambda_capture_edges(
                self.ir,
                escaping_copies
                    .iter()
                    .filter(|(source, _)| !protected.contains(source))
                    .map(|(_, copy)| *copy),
                &substituted_inline_lambdas,
            );
        let directly_spliced_lambdas = directly_spliceable_invocations(self.ir, &copies)
            .into_iter()
            .filter_map(|invocation| match self.ir.expr(invocation) {
                IrExpr::InvokeFunction { func, .. } => Some(*func),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let escaping_lambda_plan = escaping_lambda::plan(
            self.ir,
            &escaping_copies,
            &protected,
            &directly_spliced_lambdas,
            &substituted_inline_lambdas,
            &anonymous_owned_substitution_edges,
            &reified_bindings,
        )?;
        // A lambda or anonymous class the copied body declares is specialized per call site under
        // the declaration that declares it. A dependency template declares no lambda: one whose
        // plan would specialize a lambda is not a template this expansion understands. The
        // anonymous objects a template builds were lowered into this file with it, and are
        // specialized below as a same-module callee's are.
        if matches!(frame.callee, crate::ir::IrInlineCallee::Dependency(_))
            && !escaping_lambda_plan.is_empty()
        {
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

        let inline_invocations = directly_spliceable_invocations(self.ir, &copies);
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
        let default_lambda_implementation_set = default_lambda_implementations
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let mut preflight_temporary = self.next_temporary;
        for &invocation in &inline_invocations {
            preflight_temporary =
                lambda_splice_contract::preflight(self.ir, invocation, preflight_temporary)?;
        }
        // The escaping lambdas are regenerated before the anonymous objects are copied: the object
        // copies' classifier walk then renames inside each call-site lambda it reaches, which this
        // expansion alone owns. Walked first, the objects would detach the lambdas' shared
        // declaration implementations onto clones the plan never specializes, and the call-site
        // lambda would keep the reified marker. The plan commits without declining; a malformed
        // anonymous object below is a lowering failure, not a decline that leaves the call executable.
        let inlined_escaping_implementations = match frame.source() {
            Some((source_owner, callee_source)) => escaping_lambda::apply(
                self.ir,
                escaping_lambda_plan,
                &bindings,
                &reified_bindings,
                escaping_lambda::EscapingLambdaCaller {
                    declaration: caller_declaration,
                    enclosure: self.expansion_enclosure,
                    is_default: self.in_default_argument,
                    source_name: &caller_source_name,
                },
                escaping_lambda::EscapingInlineTemplate {
                    function,
                    owner: source_owner,
                    source: callee_source,
                    callee: frame.callee,
                    callee_source_name: &callee,
                },
            ),
            // The plan is empty: a dependency template declares no lambda.
            _ => Default::default(),
        };
        // An anonymous object's methods are not children of the inlined template. Cloning only the
        // `new` reuses the declaration class, whose reified parameter is erased. This is the final
        // fallible specialization: both lambda transformations above have already been preflighted,
        // and every operation after it commits without declining the containing expansion.
        // Provenance belongs to the selected argument, not to the slot staging gave it: a
        // copied operand's temporary is a fresh slot that reads nothing.
        let crossinline_operands = operands
            .iter()
            .map(|operand| operand.and_then(|operand| self.crossinline_read(operand)))
            .collect::<Vec<_>>();
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
                caller_site: self.caller_site(caller_declaration),
                inline_callee: frame.callee,
                inline_callee_source_name: &callee,
                crossinline_operands: &crossinline_operands,
                parent: None,
            },
        ) {
            *malformed_anonymous = Some(failure);
            return None;
        }
        let mut directly_spliced_implementations = HashSet::default();
        for invocation in inline_invocations {
            let crossinline = matches!(
                self.ir.expr(invocation),
                IrExpr::InvokeFunction { func, .. } if crossinline_literals.contains(func)
            );
            let consumed = match self.ir.expr(invocation) {
                IrExpr::InvokeFunction { func, .. } => match self.ir.expr(*func) {
                    IrExpr::Lambda { impl_fn, .. } => Some((
                        *impl_fn,
                        self.ir.functions[*impl_fn as usize].body,
                        self.ir.inline_only_fns.contains(impl_fn),
                    )),
                    _ => None,
                },
                _ => None,
            };
            self.splice_inline_lambda(
                invocation,
                LambdaParameterBinding::Declared {
                    callee: Some(&callee),
                    specialization: Some(&bindings),
                },
            )
            .expect("the complete declared-lambda splice list was preflighted before commit");
            if crossinline {
                if let IrExpr::Block {
                    value: Some(body), ..
                } = *self.ir.expr(invocation)
                {
                    let wrapper = self.ir.wrap_crossinline_body(body);
                    if let IrExpr::Block { value, .. } = &mut self.ir.exprs[invocation as usize] {
                        *value = Some(wrapper);
                    }
                }
            }
            if let Some((implementation, body, inline_only)) = consumed {
                // Body suppression is a commit action, not part of the splice. Restore it after
                // each successful splice so a later decline leaves the original call executable.
                self.ir.functions[implementation as usize].body = body;
                if !inline_only {
                    self.ir.inline_only_fns.remove(&implementation);
                }
                if !default_lambda_implementation_set.contains(&implementation) {
                    directly_spliced_implementations.insert(implementation);
                }
            }
        }
        for invocation in converted_unit_results {
            self.discard_converted_lambda_result(invocation);
        }
        for (function, body, inline_only) in default_lambda_methods {
            self.ir.functions[function as usize].body = body;
            if !inline_only {
                self.ir.inline_only_fns.remove(&function);
            }
        }
        // Consuming a source lambda implementation is the only irreversible part of escaping
        // inline-lambda specialization. Commit it only after every transformation that can decline
        // this expansion has succeeded; otherwise the original call still needs that body.
        // A default lambda's implementation stays a method of the declaration's default stub,
        // whichever way this expansion took its body.
        for function in inlined_escaping_implementations
            .into_iter()
            .filter(|function| !default_lambda_implementation_set.contains(function))
            .chain(directly_spliced_implementations)
        {
            self.ir.inline_only_fns.insert(function);
            self.ir.functions[function as usize].body = None;
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
            if produce_sole_tail_return(self.ir, cloned_root, tail, value, result_ty, close_line) {
                let mut statements = operand_declarations;
                statements.push(cloned_root);
                let value = statements.pop();
                let frame = self.ir.add_expr(IrExpr::Block {
                    stmts: statements,
                    value,
                });
                self.ir.mark_inline_result_boundary(frame);
                // A result declared as an unreified type parameter leaves the copied body erased;
                // the call site adapts it to the specialized type once that frame has ended.
                let declared_ret = self.ir.functions[function as usize].ret;
                if declared_ret
                    .non_null()
                    .ty_param_name()
                    .is_some_and(|name| !reified_bindings.contains_key(name))
                {
                    self.ir.record_inline_declared_type(frame, declared_ret);
                }
                return Some(frame);
            }
        }

        // The loop shape, and only now: the result local is reserved here, so a promoted expansion
        // above leaves no hole in the numbering, and each return is rewritten into the break that
        // carries it out.
        // A generic return specialized to `Unit` still carries the semantic Unit value through a
        // `finally`; a function declared to return `Unit` has no value to carry.
        let declared_ret = self.ir.functions[function as usize].ret;
        let generic_unit_result =
            result_ty == Ty::Unit && declared_ret.non_null().ty_param_name().is_some();
        let result_slot = (result_ty != Ty::Unit || generic_unit_result).then(|| {
            let slot = self.next_temporary;
            self.next_temporary += 1;
            slot
        });
        let label = frame.return_label(self.next_temporary);
        for found in returns {
            self.ir.exprs[found.returned as usize] = crate::inline_expansion::frame_exit(
                self.ir,
                &label,
                result_slot,
                found.nested,
                found.value,
                found.returned,
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
            if declared_ret
                .non_null()
                .ty_param_name()
                .is_some_and(|name| !reified_bindings.contains_key(name))
            {
                self.ir
                    .record_inline_declared_type(declaration, declared_ret);
            }
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
                frame.record_copy(self.ir, unit, function);
                if let Some(line) = close_line {
                    self.ir.set_source_line(unit, line);
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
        self.ir.set_logical_type(expression, specialized);
        expression
    }

    /// Evaluate a spliced callable reference's receiver and captures before the inline body.
    ///
    /// The reference itself is not a value: the body invokes it. The expressions that build it
    /// are still arguments of the call, so they run in that argument's place and the reference
    /// reads the stored values. A local read has already run.
    pub(super) fn stage_spliced_reference_operands(
        &mut self,
        reference: ExprId,
        declarations: &mut Vec<ExprId>,
    ) -> Option<()> {
        let IrExpr::CallableReference(mut callable) = self.ir.expr(reference).clone() else {
            return Some(());
        };
        let mut operands = Vec::new();
        if let Some(receiver) = callable.bound_receiver {
            operands.push(receiver);
        }
        operands.extend(callable.captures.iter().copied());
        if operands.is_empty() {
            return Some(());
        }
        let bound = usize::from(callable.bound_receiver.is_some());
        let mut staged = Vec::with_capacity(operands.len());
        for (ordinal, value) in operands.into_iter().enumerate() {
            if matches!(self.ir.expr(value), IrExpr::GetValue(_)) {
                staged.push(value);
                continue;
            }
            let ty = self.ir.logical_type(value)?;
            let slot = self.allocate_temporary();
            let declaration = self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty: stored_value_ty(ty),
                init: Some(value),
                named: false,
            });
            declarations.push(declaration);
            if ordinal < bound {
                self.ir
                    .inline_callable_reference_receiver_bindings
                    .insert(declaration);
            }
            let read = self.ir.add_expr(IrExpr::GetValue(slot));
            self.ir.set_logical_type(read, ty);
            staged.push(read);
        }
        if bound == 1 {
            callable.bound_receiver = Some(staged[0]);
        }
        callable.captures = staged[bound..].to_vec();
        self.ir.exprs[reference as usize] = IrExpr::CallableReference(callable);
        Some(())
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
        if !lambda_splice_contract::invocation_is_spliceable(self.ir, invocation, func) {
            return None;
        }
        let (impl_fn, captures, inline_body, indexed_parameters) = match self.ir.expr(func).clone()
        {
            IrExpr::Lambda {
                impl_fn,
                captures,
                inline_body: Some(inline_body),
                ..
            } => (impl_fn, captures, inline_body, false),
            IrExpr::CallableReference(_) => {
                let (impl_fn, captures, inline_body, _) =
                    super::source_calls::inline_argument_template(self.ir, func)?;
                // This node stays a callable reference after the invocation is replaced. It is
                // not a value: a carrier for it would keep the adapter off the inline marker.
                self.ir.inline_callable_reference_arguments.insert(func);
                (impl_fn, captures, inline_body, true)
            }
            _ => return None,
        };
        let mut parameter_types = self.ir.functions.get(impl_fn as usize)?.params.clone();
        if let LambdaParameterBinding::Declared {
            specialization: Some(bindings),
            ..
        } = binding
        {
            let formals = bindings.keys().cloned().collect::<Vec<_>>();
            for ty in &mut parameter_types {
                *ty = crate::types::ty_subst_formals(*ty, &formals, bindings);
            }
        }

        let receiver_parameter = self
            .ir
            .lambda_origins
            .get(&impl_fn)
            .and_then(|origin| origin.receiver_parameter);
        let mut declarations = Vec::new();
        // The declarations holding the invocation's own operands, after the captures'.
        let mut operand_declarations = None;
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
            if parameter == capture_count {
                operand_declarations = Some(declarations.len());
            }
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
                let reference_parameter = indexed_parameters
                    .then(|| (parameter as usize).checked_sub(capture_count))
                    .flatten();
                let source_name = if reference_parameter.is_some() {
                    None
                } else {
                    (parameter as usize >= capture_count)
                        .then(|| {
                            lambda_parameter_identities
                                .as_ref()
                                .and_then(|identities| identities.get(parameter as usize))
                                .and_then(|identity| identity.source_name.clone())
                        })
                        .flatten()
                };
                match (self.ir.expr(value), &source_name, binding) {
                    // A callable-reference parameter is `pN` even when the argument already
                    // lives in a slot. Reusing that slot would drop the local kotlinc writes.
                    (IrExpr::GetValue(slot), None, _) if reference_parameter.is_none() => *slot,
                    (IrExpr::GetValue(slot), _, LambdaParameterBinding::Remapped) => *slot,
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
                            self.ir.set_debug_local_provenance(
                                declaration,
                                IrDebugLocalProvenance::InlineLambdaParameter { depth: 0 },
                            );
                        }
                        if let Some(ordinal) = reference_parameter {
                            self.ir.set_debug_local_provenance(
                                declaration,
                                IrDebugLocalProvenance::InlineCallableReferenceParameter {
                                    ordinal: u32::try_from(ordinal).ok()?,
                                },
                            );
                        }
                        declarations.push(declaration);
                        slot
                    }
                }
            };
            formal_slots.push(slot);
        }
        let operand_declarations = operand_declarations.unwrap_or(declarations.len());
        // A suspend lambda's `invoke` takes the caller's continuation as its last operand, and the
        // splice binds it as it binds the others, to a continuation parameter of the lambda's own.
        if !indexed_parameters
            && matches!(binding, LambdaParameterBinding::Declared { .. })
            && self.ir.suspend_funs.contains(&impl_fn)
        {
            let continuation = self.ir.add_expr(IrExpr::CurrentContinuation);
            // The invocation is a suspend call, which kotlinc marks where it starts: the operand
            // it passes is read at the invocation's position, not the inlined call's.
            self.ir.copy_position(invocation, continuation);
            self.ir.copy_inline_copy_mark(invocation, continuation);
            let ty = Ty::obj("kotlin/coroutines/Continuation");
            self.ir.set_logical_type(continuation, ty);
            let slot = self.allocate_temporary();
            let declaration = self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty,
                init: Some(continuation),
                named: true,
            });
            self.ir.call_operand_bindings.insert(declaration);
            self.ir.set_debug_local_provenance(
                declaration,
                IrDebugLocalProvenance::InlineLambdaContinuation { depth: 0 },
            );
            declarations.push(declaration);
        }
        if matches!(binding, LambdaParameterBinding::Declared { .. }) {
            self.ir
                .invocation_operand_bindings
                .extend(declarations[operand_declarations..].iter().copied());
        }
        // Once its parameters are bound, the body opens its own frame, named after the inline
        // callable it was passed to.
        if let LambdaParameterBinding::Declared {
            callee: Some(callee),
            ..
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
        // An empty spliced body has no instruction after its parameter/frame locals begin. Keep
        // the invocation's semantic source point on its terminal Unit so a backend can close the
        // locals first and then anchor the surrounding inline body. Non-empty bodies already
        // restore that line through their emitted effect and must not gain a duplicate boundary.
        if let Some(unit) = unit_only_body_result(self.ir, body) {
            if let Some(line) = self.ir.source_line(invocation) {
                // With no body instruction, argument evaluation is the invocation's first
                // physical instruction. Put that source point on the generated bindings; an
                // enclosing inline copy maps it through the caller frame retained below.
                for &declaration in &declarations {
                    self.ir.set_statement_line(declaration, line);
                    self.ir.copy_inline_copy_mark(invocation, declaration);
                }
                self.ir.copy_inline_copy_mark(invocation, unit);
                self.ir.set_source_line(unit, line);
                self.ir.retain_inline_unit_line(unit);
            }
        }
        if indexed_parameters {
            if let Some(line) = self.ir.source_line(func) {
                // The adapter was built as its own method and has no source position. The splice
                // is the reference expression, so this call keeps that line.
                self.ir.set_source_line(body, line);
            }
        }
        let local_base = self.next_temporary;
        let local_count = super::source_calls::rehome_inline_body_values(
            self.ir,
            body,
            &formal_slots,
            local_base,
        )?;
        self.next_temporary = local_base.checked_add(local_count)?;
        // A callable-reference adapter may still be the invoke of a carrier realized for another
        // use of the same reference. Only a source lambda's implementation is consumed by the splice.
        if !indexed_parameters {
            self.ir.inline_only_fns.insert(impl_fn);
            self.ir.functions.get_mut(impl_fn as usize)?.body = None;
        }
        self.ir.exprs[invocation as usize] = IrExpr::Block {
            stmts: declarations,
            value: Some(body),
        };
        // The closing `nop` is the lambda literal's last line (its `}`), which is otherwise a
        // line that already holds the call and the nop cleanup would delete.
        if let Some(end) = self.ir.end_line(func) {
            self.ir.set_end_line(invocation, end);
        }
        // The spliced body's own calls are the suspension points now, not the invocation.
        self.ir.suspend_calls.remove(&invocation);
        self.ir.suspend_call_overridden_results.remove(&invocation);
        Some(())
    }
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

/// The terminal Unit of a body made only of empty block scaffolding and Unit values. Such a body
/// emits no operation of its own, unlike an ordinary Unit body whose effects restore the caller's
/// line naturally.
fn unit_only_body_result(ir: &crate::ir::IrFile, root: ExprId) -> Option<ExprId> {
    fn is_scaffolding(ir: &crate::ir::IrFile, expression: ExprId) -> bool {
        match ir.expr(expression) {
            IrExpr::UnitInstance => true,
            IrExpr::Block { stmts, value } => {
                stmts.iter().all(|&statement| is_scaffolding(ir, statement))
                    && value.is_none_or(|value| is_scaffolding(ir, value))
            }
            _ => false,
        }
    }

    let IrExpr::Block {
        value: Some(unit), ..
    } = ir.expr(root)
    else {
        return None;
    };
    (matches!(ir.expr(*unit), IrExpr::UnitInstance) && is_scaffolding(ir, root)).then_some(*unit)
}

#[cfg(test)]
mod expansion_mode_tests {
    use super::supplied_operand_plan;
    use crate::types::InlineExpansionMode;

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
