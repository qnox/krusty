//! Checked intersection facts retained across generic-call commitment.
//!
//! Ordinary expression types keep a denotable runtime view. Generic inference can additionally
//! prove that the same value inhabits several unrelated classifier views; reified diagnostics and
//! a surrounding postponed call need that complete checked fact without reconstructing it from
//! source spelling or provider origin.

use super::*;
use crate::types::TyKind;

pub(super) fn record_direct_lower_input(
    signature: Option<&crate::libraries::GenericSig>,
    declared: Ty,
    actual: Ty,
    inputs: &mut HashMap<String, Vec<Ty>>,
) {
    let Some(formal) = declared.non_null().ty_param_name() else {
        return;
    };
    if signature.is_some_and(|signature| signature.formals.iter().any(|name| name == formal)) {
        inputs.entry(formal.to_string()).or_default().push(actual);
    }
}

#[derive(Clone, Debug)]
pub(super) struct InferredIntersection {
    /// The exact committed expression type to which this fact belongs.
    pub(super) selected_result: Ty,
    /// Complete, ordered semantic constituents. The first is the inference primary; FIR stores the
    /// remainder as additional bounds.
    pub(super) parts: Vec<Ty>,
    /// Whether the fact crossed a flexible platform-type boundary.
    pub(super) through_flexible: bool,
}

/// Complete generic facts committed with one selected call.
///
/// This record is replaced as a unit whenever overload selection commits a target. Keeping the
/// substitutions, non-denotable bounds, and reified declaration identities together prevents an
/// expected-type recheck from combining facts from two different winners.
#[derive(Clone, Debug, Default)]
pub(super) struct SelectedGenericCall {
    arguments: Vec<Option<Ty>>,
    argument_provenance: Vec<ReifiedArgumentProvenance>,
    additional_bounds: Vec<Vec<Ty>>,
    pub(super) reified_parameters: Vec<(usize, crate::types::TyParamName)>,
    pub(super) intersections: HashMap<usize, InferredIntersection>,
    /// The runtime classifier of each reified slot inferred as an intersection: kotlinc reifies
    /// the whole intersection to its common supertype. Only reification reads it; the semantic
    /// argument and its additional bounds stay the checked intersection.
    reified_runtimes: HashMap<usize, Ty>,
}

impl SelectedGenericCall {
    pub(super) fn arguments(&self) -> &[Option<Ty>] {
        &self.arguments
    }

    /// A semantic call result may consume substitutions only after every declaration slot is
    /// solved. Partial evidence remains available to the checker and crosses into checked FIR as
    /// explicit `(ordinal, type)` entries; it is not a complete specialization of the call.
    fn committed_arguments(&self) -> Option<&[Option<Ty>]> {
        self.arguments
            .iter()
            .all(Option::is_some)
            .then_some(&self.arguments)
    }

    fn materialized_arguments(&self) -> impl Iterator<Item = (usize, Ty)> + '_ {
        self.arguments
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(ordinal, argument)| argument.map(|argument| (ordinal, argument)))
    }

    pub(super) fn additional_bounds(&self, ordinal: usize) -> &[Ty] {
        self.additional_bounds
            .get(ordinal)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(super) fn argument_provenance(&self, ordinal: usize) -> Option<&ReifiedArgumentProvenance> {
        self.argument_provenance.get(ordinal)
    }

    /// Return this winner's complete evidence with only newly proven substitutions applied.
    /// Bounds, intersections, and reified declaration facts belong to the current recheck and are
    /// intentionally carried through unchanged.
    fn with_completed_arguments(
        mut self,
        signature: &crate::libraries::GenericSig,
        completion_bindings: &crate::symbol_resolver::GSigBinds,
    ) -> Self {
        if self.arguments.len() < signature.formals.len() {
            self.arguments.resize(signature.formals.len(), None);
        }
        for (ordinal, formal) in signature.formals.iter().enumerate() {
            if let Some(&completion) = completion_bindings.get(formal.as_str()) {
                self.arguments[ordinal] = Some(completion);
            }
        }
        self
    }
}

/// Occurrence-level provenance needed only by inferred reified-argument diagnostics.
///
/// Generic inference deliberately normalizes a definitely-non-null or flexible input to the
/// selected denotable type. This parallel tree remembers which input boundary each occurrence
/// crossed without making source spelling or provider representation part of type identity.
#[derive(Clone, Debug, Default)]
pub(super) struct ReifiedArgumentProvenance {
    through_definitely_non_null: bool,
    through_flexible: bool,
    arguments: Vec<Self>,
}

impl ReifiedArgumentProvenance {
    pub(super) const NONE: &'static Self = &Self {
        through_definitely_non_null: false,
        through_flexible: false,
        arguments: Vec::new(),
    };

    pub(super) fn is_indirect(&self) -> bool {
        self.through_definitely_non_null || self.through_flexible
    }

    pub(super) fn crossed_flexible_boundary(&self) -> bool {
        self.through_flexible
    }

    /// Facts shared by every occurrence contributing to one inferred type argument. A direct
    /// occurrence must keep an indirect DNN/flexible occurrence from weakening the diagnostic;
    /// nested generic shapes follow the same positional rule.
    pub(super) fn meet(&self, other: &Self) -> Self {
        let arguments = self
            .arguments
            .iter()
            .zip(&other.arguments)
            .map(|(left, right)| left.meet(right))
            .collect();
        Self {
            through_definitely_non_null: self.through_definitely_non_null
                && other.through_definitely_non_null,
            through_flexible: self.through_flexible && other.through_flexible,
            arguments,
        }
    }

    pub(super) fn argument(&self, ordinal: usize) -> Option<&Self> {
        self.arguments.get(ordinal)
    }
}

#[cfg(test)]
fn reified_provenance_of_ty(ty: Ty) -> ReifiedArgumentProvenance {
    reified_provenance_of_ty_with_declaration(ty, &|_| None)
}

fn reified_provenance_of_ty_with_declaration(
    ty: Ty,
    declaration: &impl Fn(&str) -> Option<Ty>,
) -> ReifiedArgumentProvenance {
    match ty.kind() {
        TyKind::DefinitelyNotNull(inner) => {
            let mut provenance = reified_provenance_of_ty_with_declaration(inner, declaration);
            provenance.through_definitely_non_null = true;
            provenance
        }
        TyKind::PlatformNullable(inner) => {
            let mut provenance = reified_provenance_of_ty_with_declaration(inner, declaration);
            provenance.through_flexible = true;
            provenance
        }
        TyKind::Nullable(inner)
        | TyKind::InProjection(inner)
        | TyKind::OutProjection(inner)
        | TyKind::StarProjection(inner) => {
            reified_provenance_of_ty_with_declaration(inner, declaration)
        }
        TyKind::TyParam(identity, occurrence_bound) | TyKind::Var(identity, occurrence_bound) => {
            ReifiedArgumentProvenance {
                through_definitely_non_null: declaration(identity.as_str())
                    .and_then(|declared| match declared.non_null().kind() {
                        TyKind::TyParam(declared_identity, declared_bound)
                        | TyKind::Var(declared_identity, declared_bound)
                            if declared_identity == identity =>
                        {
                            Some(declared_bound)
                        }
                        _ => None,
                    })
                    .is_some_and(|declared_bound| {
                        declared_bound.upper_bound_admits_null()
                            && !occurrence_bound.upper_bound_admits_null()
                    }),
                ..Default::default()
            }
        }
        TyKind::Obj(_, arguments) => ReifiedArgumentProvenance {
            arguments: arguments
                .iter()
                .copied()
                .map(|argument| reified_provenance_of_ty_with_declaration(argument, declaration))
                .collect(),
            ..Default::default()
        },
        TyKind::Fun(signature) => ReifiedArgumentProvenance {
            arguments: signature
                .params
                .iter()
                .copied()
                .chain(std::iter::once(signature.ret))
                .map(|argument| reified_provenance_of_ty_with_declaration(argument, declaration))
                .collect(),
            ..Default::default()
        },
        _ => Default::default(),
    }
}

fn constrain_reified_argument_provenance(
    formals: &[crate::types::TyParamName],
    declared: Ty,
    actual: Ty,
    provenance: &ReifiedArgumentProvenance,
    facts: &mut HashMap<String, Vec<ReifiedArgumentProvenance>>,
) {
    if let TyKind::TyParam(formal, _) | TyKind::Var(formal, _) = declared.non_null().kind() {
        if formals.contains(&formal) {
            facts
                .entry(formal.to_string())
                .or_default()
                .push(provenance.clone());
        }
        return;
    }
    match (declared.non_null().kind(), actual.non_null().kind()) {
        (TyKind::Obj(declared_owner, declared_args), TyKind::Obj(actual_owner, actual_args))
            if declared_owner == actual_owner && declared_args.len() == actual_args.len() =>
        {
            for (index, (&declared, &actual)) in declared_args.iter().zip(actual_args).enumerate() {
                let nested = provenance.argument(index).cloned().unwrap_or_default();
                constrain_reified_argument_provenance(formals, declared, actual, &nested, facts);
            }
        }
        (TyKind::Fun(declared), TyKind::Fun(actual))
            if declared.suspend == actual.suspend
                && declared.context_count == actual.context_count
                && declared.has_receiver == actual.has_receiver
                && declared.params.len() == actual.params.len() =>
        {
            for (index, (&declared, &actual)) in declared
                .params
                .iter()
                .chain(std::iter::once(&declared.ret))
                .zip(actual.params.iter().chain(std::iter::once(&actual.ret)))
                .enumerate()
            {
                let nested = provenance.argument(index).cloned().unwrap_or_default();
                constrain_reified_argument_provenance(formals, declared, actual, &nested, facts);
            }
        }
        _ => {}
    }
}

impl TypeInfo {
    /// Fully committed selected-call type arguments in declaration order.
    pub fn resolved_call_type_arguments(&self, expression: ExprId) -> Option<&[Option<Ty>]> {
        self.selected_generic_calls
            .get(&expression)
            .and_then(SelectedGenericCall::committed_arguments)
    }
    /// Materialized substitutions with their declaration ordinals. Unsolved slots remain checker
    /// evidence only and never cross into checked FIR as a value.
    pub(crate) fn materialized_call_type_arguments(
        &self,
        expression: ExprId,
    ) -> impl Iterator<Item = (usize, Ty)> + '_ {
        self.selected_generic_calls
            .get(&expression)
            .into_iter()
            .flat_map(|call| call.materialized_arguments())
    }

    #[cfg(test)]
    pub(crate) fn selected_call_type_arguments_for_test(
        &self,
        expression: ExprId,
    ) -> Option<&[Option<Ty>]> {
        self.selected_generic_calls
            .get(&expression)
            .map(SelectedGenericCall::arguments)
    }

    #[cfg(test)]
    pub(crate) fn replace_selected_call_type_arguments_for_test(
        &mut self,
        expression: ExprId,
        arguments: Vec<Option<Ty>>,
    ) {
        let selected = self
            .selected_generic_calls
            .get_mut(&expression)
            .expect("test expression has a selected generic call");
        assert_eq!(
            selected.arguments.len(),
            arguments.len(),
            "test replacement must retain the declaration's generic arity",
        );
        selected.arguments = arguments;
    }

    /// The runtime classifier of reified slot `ordinal` of `expression`, when inference selected
    /// an intersection for it.
    pub(crate) fn resolved_call_type_argument_reified_runtime(
        &self,
        expression: ExprId,
        ordinal: usize,
    ) -> Option<Ty> {
        self.selected_generic_calls
            .get(&expression)
            .and_then(|call| call.reified_runtimes.get(&ordinal).copied())
    }

    pub(crate) fn resolved_call_type_argument_bounds(
        &self,
        expression: ExprId,
        ordinal: usize,
    ) -> &[Ty] {
        self.selected_generic_calls
            .get(&expression)
            .map(|call| call.additional_bounds(ordinal))
            .unwrap_or_default()
    }
}

fn bounds_other_than_primary(parts: &[Ty], selected_result: Ty) -> Vec<Ty> {
    parts
        .iter()
        .copied()
        .filter(|part| *part != selected_result)
        .collect()
}

impl Checker<'_> {
    /// Build reified provenance only after contextual argument checking has finalized the source
    /// expression's selected-call and flexible-value sidecars. `occurrence` is either a stable
    /// lexical declaration occurrence or the final type committed by that selected recheck.
    pub(super) fn finalized_selected_argument_reified_provenance(
        &self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        occurrence: Ty,
    ) -> ReifiedArgumentProvenance {
        let mut provenance = reified_provenance_of_ty_with_declaration(occurrence, &|identity| {
            for rung in scope.classifier_rungs() {
                let mut declared = None;
                rung.own_bindings(Ns::Classifier, |_, binding| {
                    if let ScopeBinding::TypeParam { bound, .. } = binding {
                        if bound.non_null().ty_param_name() == Some(identity) {
                            declared = Some(*bound);
                        }
                    }
                });
                if declared.is_some() {
                    return declared;
                }
            }
            None
        });
        if self.checked_value_is_flexible(expression) {
            provenance.through_flexible = true;
        }
        provenance
    }

    fn selected_reified_argument_provenance(
        &self,
        call: ExprId,
        signature: &crate::libraries::GenericSig,
        parameter_indices: &[usize],
    ) -> Vec<ReifiedArgumentProvenance> {
        let mut facts: HashMap<String, Vec<ReifiedArgumentProvenance>> = HashMap::default();
        if let Some(arguments) = self.resolved_call_arg_slots.get(&call) {
            for binding in &arguments.argument_bindings {
                let Some(&parameter) = parameter_indices.get(binding.parameter) else {
                    continue;
                };
                let Some(&declared) = signature.params.get(parameter) else {
                    continue;
                };
                let declared = if binding.vararg_element {
                    declared.array_read_elem().unwrap_or(declared)
                } else {
                    declared
                };
                constrain_reified_argument_provenance(
                    &signature.formals,
                    declared,
                    binding.reified_occurrence(self.expr_types[binding.argument.0 as usize]),
                    &binding.source_provenance,
                    &mut facts,
                );
            }
        }
        signature
            .formals
            .iter()
            .map(|formal| {
                let Some(values) = facts.get(formal.as_str()) else {
                    return Default::default();
                };
                values
                    .iter()
                    .skip(1)
                    .fold(values[0].clone(), |known, next| known.meet(next))
            })
            .collect()
    }

    pub(super) fn selected_argument_lower_inputs(
        &self,
        call: ExprId,
        signature: &crate::libraries::GenericSig,
        parameter_indices: &[usize],
    ) -> HashMap<String, Vec<Ty>> {
        let mut inputs = HashMap::default();
        if let Some(commitment) = self.resolved_call_arg_slots.get(&call) {
            for binding in &commitment.argument_bindings {
                let Some(&parameter) = parameter_indices.get(binding.parameter) else {
                    continue;
                };
                let Some(&declared) = signature.params.get(parameter) else {
                    continue;
                };
                record_direct_lower_input(
                    Some(signature),
                    declared,
                    self.expr_types[binding.argument.0 as usize],
                    &mut inputs,
                );
            }
        }
        inputs
    }

    pub(super) fn positional_lower_inputs(
        &self,
        signature: &crate::libraries::GenericSig,
        actuals: &[Ty],
    ) -> HashMap<String, Vec<Ty>> {
        let mut inputs = HashMap::default();
        for (&declared, &actual) in signature.params.iter().zip(actuals) {
            record_direct_lower_input(Some(signature), declared, actual, &mut inputs);
        }
        inputs
    }

    pub(super) fn intersection_bindings_from_lower_inputs(
        &self,
        inputs: &HashMap<String, Vec<Ty>>,
    ) -> HashMap<String, Vec<Ty>> {
        inputs
            .iter()
            .filter_map(|(formal, inputs)| {
                (inputs.len() >= 2)
                    .then(|| self.minimal_common_classifier_views_many(inputs))
                    .filter(|parts| parts.len() > 1)
                    .map(|parts| (formal.clone(), parts))
            })
            .collect()
    }

    pub(super) fn selected_call_type_arguments(&self, call: ExprId) -> Option<&[Option<Ty>]> {
        self.selected_generic_calls
            .get(&call)
            .map(SelectedGenericCall::arguments)
    }

    /// Replace substitutions for a call shape that has no callable declaration carrying reified
    /// formals (constructors, callable references, and compiler-provided adapters).
    pub(super) fn replace_call_type_arguments(&mut self, call: ExprId, arguments: Vec<Option<Ty>>) {
        if arguments.is_empty() {
            self.clear_selected_generic_call(call);
            return;
        }
        self.inferred_expression_intersections.remove(&call);
        self.selected_generic_calls.insert(
            call,
            SelectedGenericCall {
                arguments,
                ..Default::default()
            },
        );
    }

    /// The common supertype kotlinc reifies `intersection` to, walking every constituent's
    /// declared hierarchy through the same oracle as assignability, so a dependency classifier's
    /// supertypes count as a source one's do.
    fn reified_intersection_runtime(&self, intersection: &InferredIntersection) -> Ty {
        let parts = intersection
            .parts
            .iter()
            .map(|part| part.non_null())
            .collect::<Vec<_>>();
        let checked = Ty::intersection(&parts);
        let checked = if intersection.selected_result.is_nullable() {
            Ty::nullable(checked)
        } else {
            checked
        };
        crate::symbol_resolver::reified_runtime_type(checked, &|name| {
            Some(
                crate::assignable::TypeOracle::direct_supertypes(self, Ty::obj_name(name))
                    .into_iter()
                    .filter_map(|supertype| supertype.obj_internal())
                    .collect(),
            )
        })
    }

    fn reified_parameters(
        signature: &crate::libraries::GenericSig,
        call_sig: &CallSig,
    ) -> Vec<(usize, crate::types::TyParamName)> {
        call_sig
            .reified_type_parameter_ordinals
            .iter()
            .filter_map(|&ordinal| {
                let ordinal = usize::try_from(ordinal).ok()?;
                Some((ordinal, *signature.formals.get(ordinal)?))
            })
            .collect()
    }

    /// A selected non-generic target replaces every generic fact from an earlier contextual pass.
    pub(super) fn clear_selected_generic_call(&mut self, call: ExprId) {
        self.selected_generic_calls.remove(&call);
        self.inferred_expression_intersections.remove(&call);
    }

    /// Replace a selected call with a generic declaration whose inference has no retained
    /// intersection constituents. This is the common commitment seam for member, extension,
    /// indexed-operator, and other origins that already finalized their substitutions.
    pub(super) fn replace_selected_generic_call(
        &mut self,
        call: ExprId,
        signature: &crate::libraries::GenericSig,
        call_sig: &CallSig,
        arguments: Vec<Option<Ty>>,
    ) {
        self.inferred_expression_intersections.remove(&call);
        let parameter_indices = (0..signature.params.len()).collect::<Vec<_>>();
        let argument_provenance =
            self.selected_reified_argument_provenance(call, signature, &parameter_indices);
        self.selected_generic_calls.insert(
            call,
            SelectedGenericCall {
                arguments,
                argument_provenance,
                reified_parameters: Self::reified_parameters(signature, call_sig),
                ..Default::default()
            },
        );
    }

    /// Apply contextual completion only when the recheck retained the declaration-owned generic
    /// shape that produced it. The complete current winner is cloned after that guard and replaced
    /// atomically; no field from the provisional commitment is carried across the recheck.
    pub(super) fn replace_contextually_completed_generic_call(
        &mut self,
        call: ExprId,
        provisional: &super::call_result_templates::SelectedGenericCallShape,
        selected: &super::call_result_templates::SelectedGenericCallShape,
        completion_bindings: &crate::symbol_resolver::GSigBinds,
    ) {
        if !provisional.can_reapply_completion_to(selected) {
            return;
        }
        let Some(current) = self.selected_generic_calls.get(&call).cloned() else {
            return;
        };
        let completed = current.with_completed_arguments(selected.signature(), completion_bindings);
        self.selected_generic_calls.insert(call, completed);
    }

    /// Commit inference owned by an ordinary module member after overload selection has chosen the
    /// declaration. Keeping the record construction here gives every member origin the same
    /// replacement boundary instead of scattering selected-call sidecars through the resolver
    /// facade.
    pub(super) fn commit_module_member_generic_call(
        &mut self,
        call: ExprId,
        signature: Option<&crate::libraries::GenericSig>,
        inferred_arguments: Option<Vec<Option<Ty>>>,
        explicit_type_args: &[Ty],
        logical_arg_tys: &[Ty],
        call_sig: &CallSig,
        mut ret: Ty,
    ) -> Ty {
        let Some(signature) = signature else {
            self.clear_selected_generic_call(call);
            return ret;
        };
        let arguments = if explicit_type_args.is_empty() {
            inferred_arguments.unwrap_or_else(|| vec![None; signature.formals.len()])
        } else {
            explicit_type_args.iter().copied().map(Some).collect()
        };
        let mut bindings = signature
            .formals
            .iter()
            .zip(&arguments)
            .filter_map(|(formal, argument)| {
                argument.map(|argument| (crate::types::TyParamName::from(formal), argument))
            })
            .collect::<crate::symbol_resolver::GSigBinds>();
        let lower_inputs = self.positional_lower_inputs(signature, logical_arg_tys);
        let mut intersection_bindings = self.intersection_bindings_from_lower_inputs(&lower_inputs);
        let parameter_indices = (0..signature.params.len()).collect::<Vec<_>>();
        ret = self.commit_selected_call_intersections(
            call,
            signature,
            explicit_type_args,
            &parameter_indices,
            call_sig,
            &mut bindings,
            &mut intersection_bindings,
            Default::default(),
            ret,
        );
        ret
    }

    /// Publish the complete generic commitment of an already selected member. This is bookkeeping
    /// over the winner's normalized declaration, never another lookup or selection pass.
    pub(super) fn commit_resolved_member_generic_call(
        &mut self,
        call: ExprId,
        selected: &crate::symbol_resolver::ResolvedMember,
        parameter_indices: &[usize],
        call_sig: &CallSig,
        explicit_type_args: &[Ty],
        mut ret: Ty,
    ) -> Ty {
        let Some(signature) = selected.member.generic_sig.as_ref() else {
            if explicit_type_args.is_empty() {
                self.clear_selected_generic_call(call);
            } else {
                self.replace_call_type_arguments(
                    call,
                    explicit_type_args
                        .iter()
                        .copied()
                        .map(|argument| (argument != Ty::Error).then_some(argument))
                        .collect(),
                );
            }
            return ret;
        };

        let mut bindings = self.member_receiver_type_bindings(selected.receiver, &selected.member);
        let source = self.fed_source();
        for (&declared, &actual) in signature.params.iter().zip(&selected.member.params) {
            crate::symbol_resolver::unify_ty_from_symbols(&source, declared, actual, &mut bindings);
        }
        if ret != Ty::Error && !ret.mentions_pending() {
            crate::symbol_resolver::unify_ty_from_symbols(
                &source,
                signature.ret,
                ret,
                &mut bindings,
            );
        }
        let lower_inputs = self.selected_argument_lower_inputs(call, signature, parameter_indices);
        let mut intersection_bindings = self.intersection_bindings_from_lower_inputs(&lower_inputs);
        ret = self.commit_selected_call_intersections(
            call,
            signature,
            explicit_type_args,
            parameter_indices,
            call_sig,
            &mut bindings,
            &mut intersection_bindings,
            Default::default(),
            ret,
        );
        ret
    }

    pub(super) fn inferred_expression_intersection(&self, expression: ExprId) -> Vec<Ty> {
        self.inferred_expression_intersection_fact(expression)
            .map(|fact| fact.parts)
            .unwrap_or_default()
    }

    pub(super) fn inferred_expression_intersection_fact(
        &self,
        expression: ExprId,
    ) -> Option<InferredIntersection> {
        self.inferred_expression_intersections
            .get(&expression)
            .cloned()
    }

    /// Retain the complete common classifier facets of every normally completing value edge.
    /// Nested conditional facts are edge input, so an inner non-denotable join is not collapsed to
    /// its denotable primary before an outer `if`, `when`, elvis, or `try` joins it again.
    pub(super) fn publish_conditional_intersection(
        &mut self,
        expression: ExprId,
        edges: impl IntoIterator<Item = (ExprId, Ty)>,
        selected_result: Ty,
    ) {
        let mut through_flexible = false;
        let edges = edges
            .into_iter()
            .filter(|(_, ty)| !matches!(*ty, Ty::Nothing | Ty::Error))
            .map(|(expression, ty)| {
                let value =
                    super::conditional_branch::branch_value_expression(self.file, expression);
                let fact = self.inferred_expression_intersection_fact(value);
                if let Some(fact) = fact.filter(|fact| {
                    fact.selected_result == ty
                        || (fact.through_flexible
                            && fact.selected_result.non_null() == ty.non_null())
                }) {
                    through_flexible |= fact.through_flexible;
                    fact.parts
                } else {
                    vec![ty]
                }
            })
            .collect::<Vec<_>>();
        let parts = self.minimal_common_classifier_views_for_edges(&edges);
        if parts.len() > 1 {
            self.inferred_expression_intersections.insert(
                expression,
                InferredIntersection {
                    selected_result,
                    parts,
                    through_flexible,
                },
            );
        } else {
            self.inferred_expression_intersections.remove(&expression);
        }
    }

    /// Retain a finalized postponed argument's intersection only when that fact remains the exact
    /// solution of a direct formal and every other direct argument proves every constituent.
    /// Successful propagation normalizes the formal to the first constituent, matching FIR's
    /// primary/additional-bound contract.
    pub(super) fn retain_postponed_argument_intersections(
        &self,
        call: ExprId,
        signature: &crate::libraries::GenericSig,
        explicit_type_args: &[Ty],
        parameter_indices: &[usize],
        call_sig: &CallSig,
        selected_bindings: &mut crate::symbol_resolver::GSigBinds,
        intersection_bindings: &mut HashMap<String, Vec<Ty>>,
    ) -> HashMap<String, InferredIntersection> {
        let mut propagated = HashMap::default();
        let Some(slots) = self.resolved_call_arg_slots.get(&call) else {
            return propagated;
        };
        for (formal_index, formal) in signature.formals.iter().enumerate() {
            if explicit_type_args
                .get(formal_index)
                .is_some_and(|argument| *argument != Ty::Error)
            {
                continue;
            }
            let Some(&selected_argument) = selected_bindings.get(formal.as_str()) else {
                continue;
            };
            let direct_arguments = slots
                .argument_bindings
                .iter()
                .filter(|binding| {
                    let visible_parameter = binding.parameter;
                    let Some(&parameter) = parameter_indices.get(visible_parameter) else {
                        return false;
                    };
                    if call_sig.vararg_index == Some(visible_parameter)
                        || call_sig
                            .exact_params
                            .get(visible_parameter)
                            .copied()
                            .unwrap_or(false)
                        || signature
                            .params
                            .get(parameter)
                            .copied()
                            .map(Ty::non_null)
                            .and_then(Ty::ty_param_name)
                            != Some(formal.as_str())
                    {
                        return false;
                    }
                    true
                })
                .collect::<Vec<_>>();
            for argument in &direct_arguments {
                let Some(fact) = argument.source_intersection.clone() else {
                    continue;
                };
                // A flexible producer is committed to a non-null generic parameter through its
                // lower bound. That is the same selected value, not a widening: retain the
                // boundary provenance while normalizing the call's denotable type argument.
                let selected_same_value = fact.selected_result == selected_argument
                    || (fact.through_flexible
                        && fact.selected_result.non_null() == selected_argument);
                if fact.parts.len() < 2 || !selected_same_value {
                    continue;
                }
                let all_arguments_prove_parts = direct_arguments.iter().all(|other| {
                    let actual = self.expr_types[other.argument.0 as usize];
                    fact.parts.iter().all(|&part| {
                        self.receiver_is_assignable(actual, part)
                            || other.source_intersection.as_ref().is_some_and(|other| {
                                other
                                    .parts
                                    .iter()
                                    .copied()
                                    .any(|view| self.receiver_is_assignable(view, part))
                            })
                    })
                });
                if !all_arguments_prove_parts {
                    continue;
                }
                selected_bindings.insert(*formal, fact.parts[0]);
                intersection_bindings.insert(formal.to_string(), fact.parts.clone());
                propagated.insert(formal.to_string(), fact);
                break;
            }
        }
        propagated
    }

    /// Commit the type arguments and intersection provenance of an already-selected generic call.
    /// Every call representation uses this same semantic boundary; whether lowering records the
    /// declaration as top-level, static, or a member must not change inference.
    pub(super) fn commit_selected_call_intersections(
        &mut self,
        call: ExprId,
        signature: &crate::libraries::GenericSig,
        explicit_type_args: &[Ty],
        parameter_indices: &[usize],
        call_sig: &CallSig,
        selected_bindings: &mut crate::symbol_resolver::GSigBinds,
        intersection_bindings: &mut HashMap<String, Vec<Ty>>,
        return_info: crate::libraries::ReturnInfo,
        mut final_result: Ty,
    ) -> Ty {
        let propagated = self.retain_postponed_argument_intersections(
            call,
            signature,
            explicit_type_args,
            parameter_indices,
            call_sig,
            selected_bindings,
            intersection_bindings,
        );
        let argument_provenance = if explicit_type_args.is_empty() {
            self.selected_reified_argument_provenance(call, signature, parameter_indices)
        } else {
            vec![Default::default(); signature.formals.len()]
        };
        if !propagated.is_empty() {
            let inferred_return =
                crate::symbol_resolver::ty_subst_keep_unbound(signature.ret, selected_bindings);
            final_result =
                return_info.apply(signature.apply_return_policy(self.libraries, inferred_return));
        }
        let resolved = signature
            .formals
            .iter()
            .enumerate()
            .map(|(index, formal)| {
                explicit_type_args
                    .get(index)
                    .copied()
                    .filter(|argument| *argument != Ty::Error)
                    .or_else(|| selected_bindings.get(formal.as_str()).copied())
            })
            .collect::<Vec<_>>();
        self.publish_selected_call_intersections(
            call,
            signature,
            call_sig,
            resolved,
            selected_bindings,
            intersection_bindings,
            &propagated,
            argument_provenance,
            final_result,
        );
        final_result
    }

    /// Publish one selected call's exact intersection facts after its final result policy and type
    /// arguments have committed. Rechecks replace all prior facts for this call.
    pub(super) fn publish_selected_call_intersections(
        &mut self,
        call: ExprId,
        signature: &crate::libraries::GenericSig,
        call_sig: &CallSig,
        arguments: Vec<Option<Ty>>,
        selected_bindings: &crate::symbol_resolver::GSigBinds,
        intersection_bindings: &HashMap<String, Vec<Ty>>,
        propagated: &HashMap<String, InferredIntersection>,
        argument_provenance: Vec<ReifiedArgumentProvenance>,
        final_result: Ty,
    ) {
        self.clear_selected_generic_call(call);
        let mut additional_bounds = vec![Vec::new(); signature.formals.len()];
        let mut intersections = HashMap::default();
        for (ordinal, formal) in signature.formals.iter().enumerate() {
            let Some(parts) = intersection_bindings
                .get(formal.as_str())
                .filter(|parts| parts.len() > 1)
            else {
                continue;
            };
            let selected_result = selected_bindings
                .get(formal.as_str())
                .copied()
                .unwrap_or(parts[0]);
            let through_flexible = propagated
                .get(formal.as_str())
                .is_some_and(|fact| fact.through_flexible);
            intersections.insert(
                ordinal,
                InferredIntersection {
                    selected_result,
                    parts: parts.clone(),
                    through_flexible,
                },
            );
            additional_bounds[ordinal].extend(bounds_other_than_primary(parts, selected_result));
        }
        let reified_parameters = Self::reified_parameters(signature, call_sig);
        let reified_runtimes = reified_parameters
            .iter()
            .filter_map(|(ordinal, _)| {
                let intersection = intersections.get(ordinal)?;
                Some((*ordinal, self.reified_intersection_runtime(intersection)))
            })
            .collect();
        self.selected_generic_calls.insert(
            call,
            SelectedGenericCall {
                arguments,
                argument_provenance,
                additional_bounds,
                reified_parameters,
                intersections,
                reified_runtimes,
            },
        );

        let result_formal = match signature.ret.kind() {
            TyKind::TyParam(formal, _) | TyKind::Var(formal, _) => Some(formal.as_str()),
            TyKind::Nullable(inner)
            | TyKind::PlatformNullable(inner)
            | TyKind::MutabilityFlexible(inner) => inner.ty_param_name(),
            _ => None,
        };
        let Some(formal) = result_formal else {
            return;
        };
        let Some(parts) = intersection_bindings
            .get(formal)
            .filter(|parts| parts.len() > 1)
        else {
            return;
        };
        self.inferred_expression_intersections.insert(
            call,
            InferredIntersection {
                selected_result: final_result,
                parts: parts.clone(),
                through_flexible: final_result.is_platform_nullable()
                    || signature.ret.is_platform_nullable()
                    || propagated
                        .get(formal)
                        .is_some_and(|fact| fact.through_flexible),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspend_function_provenance_requires_the_same_callable_shape() {
        let formal_name = "sample:consume:X".to_string();
        let bound = Ty::nullable(Ty::obj("kotlin/Any"));
        let formal = Ty::ty_param(&formal_name, bound);
        let occurrence =
            Ty::ty_param("sample:caller:T", bound).contributed_through_nullable_formal();
        let declared = Ty::fun_suspend(vec![formal], Ty::Unit);
        let actual = Ty::fun_suspend(vec![occurrence], Ty::Unit);
        let provenance = reified_provenance_of_ty(actual);
        let formals = vec![crate::types::TyParamName::new(&formal_name)];
        let mut facts = HashMap::default();

        constrain_reified_argument_provenance(&formals, declared, actual, &provenance, &mut facts);

        let [retained] = facts[&formal_name].as_slice() else {
            panic!("the matching suspend parameter must retain one occurrence")
        };
        assert!(retained.through_definitely_non_null);

        let mut mismatched = HashMap::default();
        constrain_reified_argument_provenance(
            &formals,
            declared,
            Ty::fun(vec![occurrence], Ty::Unit),
            &provenance,
            &mut mismatched,
        );
        assert!(mismatched.is_empty());
    }

    #[test]
    fn source_dnn_provenance_matches_semantic_identity_at_root_and_nested_occurrences() {
        let identity = "sample:caller:T";
        let nullable_bound = Ty::nullable(Ty::obj("kotlin/Any"));
        let declaration = Ty::ty_param(identity, nullable_bound);
        let occurrence = Ty::ty_param(identity, Ty::obj("kotlin/Any"));
        let declared = |candidate: &str| (candidate == identity).then_some(declaration);

        let root = reified_provenance_of_ty_with_declaration(occurrence, &declared);
        assert!(root.is_indirect());

        let nested = reified_provenance_of_ty_with_declaration(
            Ty::fun(vec![occurrence], Ty::Unit),
            &declared,
        );
        assert!(nested
            .argument(0)
            .is_some_and(|argument| argument.is_indirect()));
        assert!(!nested.is_indirect());

        let unrelated = reified_provenance_of_ty_with_declaration(occurrence, &|_| {
            Some(Ty::ty_param("sample:other:T", nullable_bound))
        });
        assert!(!unrelated.is_indirect());
    }

    #[test]
    fn additional_bounds_exclude_the_committed_primary_by_identity() {
        let first = Ty::obj("sample/First");
        let primary = Ty::obj("sample/Primary");
        let last = Ty::obj("sample/Last");
        assert_eq!(
            bounds_other_than_primary(&[first, primary, last], primary),
            vec![first, last]
        );
    }

    #[test]
    fn contextual_completion_preserves_the_current_winners_unrelated_evidence() {
        let formal = crate::types::TyParamName::new("sample:produce:T");
        let retained_bound = Ty::obj("sample/Retained");
        let retained_intersection = InferredIntersection {
            selected_result: Ty::obj("sample/Primary"),
            parts: vec![Ty::obj("sample/Primary"), retained_bound],
            through_flexible: true,
        };
        let mut intersections = HashMap::default();
        intersections.insert(0, retained_intersection);
        let current = SelectedGenericCall {
            arguments: vec![None, Some(Ty::String)],
            argument_provenance: Vec::new(),
            additional_bounds: vec![vec![retained_bound], Vec::new()],
            reified_parameters: vec![(1, crate::types::TyParamName::new("sample:produce:U"))],
            intersections,
            reified_runtimes: HashMap::default(),
        };
        let signature = crate::libraries::GenericSig {
            formals: vec![formal, crate::types::TyParamName::new("sample:produce:U")],
            formal_bounds: vec![Vec::new(), Vec::new()],
            receiver: None,
            params: Vec::new(),
            ret: Ty::ty_param(&formal, Ty::nullable(Ty::obj("kotlin/Any"))),
            return_policy: crate::libraries::GenericReturnPolicy::Exact,
        };
        let mut completion = crate::symbol_resolver::GSigBinds::default();
        completion.insert(formal, Ty::Int);

        let provisional = super::call_result_templates::SelectedGenericCallShape::source_for_test(
            81,
            signature.clone(),
        );
        let selected = super::call_result_templates::SelectedGenericCallShape::source_for_test(
            81,
            signature.clone(),
        );
        assert!(provisional.can_reapply_completion_to(&selected));

        let completed = current.with_completed_arguments(&signature, &completion);

        assert_eq!(completed.arguments, vec![Some(Ty::Int), Some(Ty::String)]);
        assert_eq!(
            completed.additional_bounds,
            vec![vec![retained_bound], Vec::new()]
        );
        assert_eq!(
            completed.reified_parameters,
            vec![(1, crate::types::TyParamName::new("sample:produce:U"))]
        );
        let retained = completed
            .intersections
            .get(&0)
            .expect("retained intersection");
        assert_eq!(
            retained.parts,
            vec![Ty::obj("sample/Primary"), retained_bound]
        );
        assert!(retained.through_flexible);
    }

    #[test]
    fn only_materialized_type_arguments_cross_the_checked_fir_boundary() {
        let partial = SelectedGenericCall {
            arguments: vec![Some(Ty::String), None],
            ..Default::default()
        };
        assert_eq!(partial.arguments(), &[Some(Ty::String), None]);
        assert_eq!(partial.committed_arguments(), None);
        assert_eq!(
            partial.materialized_arguments().collect::<Vec<_>>(),
            [(0, Ty::String)]
        );

        let complete = SelectedGenericCall {
            arguments: vec![Some(Ty::String), Some(Ty::Int)],
            ..Default::default()
        };
        assert_eq!(
            complete.committed_arguments(),
            Some([Some(Ty::String), Some(Ty::Int)].as_slice())
        );
        assert_eq!(
            complete.materialized_arguments().collect::<Vec<_>>(),
            [(0, Ty::String), (1, Ty::Int)]
        );
    }
}
