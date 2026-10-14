//! Scheduling and memo ownership for postponed signature-call arguments.

use super::*;

/// Whether a postponed lambda can contribute constraints without concrete value-parameter
/// expectations. Written value types, or contextual value parameters the body never reads, make
/// its body independently checkable once contextual/receiver inputs are concrete.
fn lambda_can_contribute_independently(
    argument: &SigCallArgument,
    expectation: Option<ResolvedTy>,
    graph: &SignatureGraph,
) -> bool {
    let Some(SigExpr::ContextualFunction {
        parameters, result, ..
    }) = graph.expr(argument.value)
    else {
        return false;
    };
    let contextual_parameters = graph
        .operands(parameters)
        .iter()
        .copied()
        .filter(|parameter| {
            matches!(
                graph.expr(*parameter),
                Some(SigExpr::ContextualParameter(_))
            )
        })
        .collect::<Vec<_>>();
    if graph.expression_reaches_any(result, &contextual_parameters) {
        return false;
    }
    let Some(expectation) = expectation else {
        return true;
    };
    let Ty::Fun(signature) = expectation.get().non_null() else {
        return false;
    };
    let contextual_inputs =
        signature.context_count.min(signature.params.len()) + usize::from(signature.has_receiver);
    signature.params[..contextual_inputs.min(signature.params.len())]
        .iter()
        .all(|parameter| {
            !parameter.mentions_ty_param()
                && !parameter.mentions_pending()
                && !parameter.mentions_error()
        })
}

/// Order postponed arguments by their ability to publish constraints: independently typed
/// lambdas, then callable references, then concrete inputs and the remaining source order.
///
/// Candidate shaping may approximate an unresolved declaration formal to `Any` or its bound, so
/// an expectation that merely looks concrete cannot outrank source-owned evidence. A written
/// lambda parameter/result can publish constraints without that approximation; an unambiguous
/// callable reference can do the same, while a failed overloaded reference stays transactional
/// and lets the next argument contribute.
pub(super) fn argument_order(
    probes: &[SigCallArgumentProbe<'_>],
    expectations: &[Option<ResolvedTy>],
    arguments: &[SigCallArgument],
    graph: &SignatureGraph,
) -> Vec<usize> {
    let postponed = probes
        .iter()
        .enumerate()
        .filter_map(|(index, probe)| {
            matches!(
                probe,
                SigCallArgumentProbe::PostponedLambda { .. }
                    | SigCallArgumentProbe::PostponedCallableReference { .. }
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let has_concrete_inputs = |index: usize| {
        let Some(expected) = expectations.get(index).copied().flatten() else {
            return false;
        };
        let Ty::Fun(signature) = expected.get().non_null() else {
            return false;
        };
        signature.params.iter().all(|parameter| {
            !parameter.mentions_ty_param()
                && !parameter.mentions_pending()
                && !parameter.mentions_error()
        })
    };
    let mut ordered = Vec::with_capacity(postponed.len());
    for &index in &postponed {
        if lambda_can_contribute_independently(
            &arguments[index],
            expectations.get(index).copied().flatten(),
            graph,
        ) {
            ordered.push(index);
        }
    }
    for &index in &postponed {
        if !ordered.contains(&index)
            && matches!(
                probes[index],
                SigCallArgumentProbe::PostponedCallableReference { .. }
            )
        {
            ordered.push(index);
        }
    }
    for &index in &postponed {
        if !ordered.contains(&index) && has_concrete_inputs(index) {
            ordered.push(index);
        }
    }
    for index in postponed {
        if !ordered.contains(&index) {
            ordered.push(index);
        }
    }
    ordered
}

/// Run one provisional evaluation as a transaction over shared graph-walk and semantic state.
/// Failed attempts restore every store; successful ones report the memo entries they own for later
/// invalidation.
pub(super) fn transactional<S: PostponedArgumentTransaction, R, E>(
    semantics: &S,
    memo: &mut HashMap<SigExprId, ResolvedTy>,
    computing: &mut crate::fx_hash::HashSet<SigExprId>,
    evaluate: impl FnOnce(
        &mut HashMap<SigExprId, ResolvedTy>,
        &mut crate::fx_hash::HashSet<SigExprId>,
    ) -> Result<R, E>,
) -> Result<(R, Vec<SigExprId>), E> {
    let memo_before = memo.clone();
    let computing_before = computing.clone();
    semantics.begin_postponed_argument_transaction();
    match evaluate(memo, computing) {
        Ok(resolved) => {
            let entries = memo
                .keys()
                .filter(|expression| !memo_before.contains_key(expression))
                .copied()
                .collect();
            semantics.commit_postponed_argument_transaction();
            Ok((resolved, entries))
        }
        Err(error) => {
            *memo = memo_before;
            *computing = computing_before;
            semantics.rollback_postponed_argument_transaction();
            Err(error)
        }
    }
}

/// Per-call ownership for provisional argument expectations and their memoized subtrees.
pub(super) struct Scheduler {
    originally_postponed: Vec<bool>,
    cross_argument_refinement: bool,
    callable_references: Vec<bool>,
    expectations: Vec<Option<Option<ResolvedTy>>>,
    expectation_history: Vec<Vec<Option<ResolvedTy>>>,
    memo_entries: Vec<Vec<SigExprId>>,
}

impl Scheduler {
    pub(super) fn new(probes: &[SigCallArgumentProbe<'_>]) -> Self {
        let postponed_count = probes
            .iter()
            .filter(|probe| {
                matches!(
                    probe,
                    SigCallArgumentProbe::PostponedLambda { .. }
                        | SigCallArgumentProbe::PostponedCallableReference { .. }
                )
            })
            .count();
        Self {
            originally_postponed: probes
                .iter()
                .map(|probe| {
                    matches!(
                        probe,
                        SigCallArgumentProbe::PostponedLambda { .. }
                            | SigCallArgumentProbe::PostponedCallableReference { .. }
                    )
                })
                .collect(),
            // A call with several postponed arguments owns a possible sibling-constraint system.
            // Do not infer that ownership from the first materialized expectation: candidate
            // selection may already have approximated its declaration formal to `Any` or a bound.
            // Exact input-shape history below still makes concrete, independent siblings one-shot,
            // while retaining the final expectation pass needed when another sibling specializes a
            // declaration formal.
            cross_argument_refinement: postponed_count > 1,
            callable_references: probes
                .iter()
                .map(|probe| {
                    matches!(
                        probe,
                        SigCallArgumentProbe::PostponedCallableReference { .. }
                    )
                })
                .collect(),
            expectations: vec![None; probes.len()],
            expectation_history: vec![Vec::new(); probes.len()],
            memo_entries: vec![Vec::new(); probes.len()],
        }
    }

    pub(super) fn record(
        &mut self,
        index: usize,
        expectation: Option<ResolvedTy>,
        memo_entries: Vec<SigExprId>,
    ) {
        self.expectations[index] = Some(expectation);
        self.expectation_history[index].push(expectation);
        self.memo_entries[index] = memo_entries;
    }

    pub(super) fn has_cross_argument_refinement(&self) -> bool {
        self.cross_argument_refinement
    }

    /// Whether a final all-typed expectation pass still belongs to an active sibling refinement
    /// system. The semantic adapter normally stops contextual lookup after materialization, but a
    /// productive last sibling can specialize an earlier lambda's input only at this boundary.
    pub(super) fn requires_typed_expectations(&self) -> bool {
        self.cross_argument_refinement
    }

    /// First argument whose current expectation has specialized to a shape not visited before.
    /// Remembering every visited shape prevents rejected/cyclic overload families from oscillating.
    pub(super) fn refresh_candidate(
        &self,
        materialized: &[Option<ResolvedSigCallArgument<'_>>],
        expectations: &[Option<ResolvedTy>],
    ) -> Option<usize> {
        // Refresh exists for information published by a sibling postponed argument. With only one
        // postponed argument, every changed expectation is feedback from that same argument and
        // its already-checked body is the evidence the call must consume.
        if !self.cross_argument_refinement {
            return None;
        }
        self.originally_postponed
            .iter()
            .enumerate()
            .find_map(|(index, &postponed)| {
                // The final fixed-point request projects the selected callable's current
                // parameter vector back onto every originally postponed argument. An absent
                // expectation still cannot be a new input shape.
                let expectation = expectations.get(index).copied().flatten()?;
                let previous = self.expectations[index].flatten();
                // A lambda has already published its body result. Substituting that result into
                // the callable expectation is a constraint update, not a reason to consume the
                // contextual body a second time; only a changed receiver/context/value input can
                // change what the body means. A callable reference is different: its complete
                // expected signature selects the referenced overload, including by result type.
                let input_shape_changed = match (
                    previous.map(ResolvedTy::get).map(Ty::non_null),
                    Some(expectation.get().non_null()),
                ) {
                    (Some(Ty::Fun(previous)), Some(Ty::Fun(current))) => {
                        previous.params != current.params
                            || previous.context_count != current.context_count
                            || previous.has_receiver != current.has_receiver
                            || previous.suspend != current.suspend
                    }
                    _ => previous != Some(expectation),
                };
                let shape_changed = if self.callable_references[index] {
                    previous != Some(expectation)
                } else {
                    input_shape_changed
                };
                (postponed
                    && materialized[index].is_some()
                    && shape_changed
                    && !self.expectation_history[index].contains(&Some(expectation)))
                .then_some(index)
            })
    }

    pub(super) fn invalidate(&mut self, index: usize, memo: &mut HashMap<SigExprId, ResolvedTy>) {
        for expression in self.memo_entries[index].drain(..) {
            memo.remove(&expression);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct SemanticState {
        value: Cell<u32>,
        checkpoints: RefCell<Vec<u32>>,
    }

    impl PostponedArgumentTransaction for SemanticState {
        fn begin_postponed_argument_transaction(&self) {
            self.checkpoints.borrow_mut().push(self.value.get());
        }

        fn commit_postponed_argument_transaction(&self) {
            self.checkpoints.borrow_mut().pop().unwrap();
        }

        fn rollback_postponed_argument_transaction(&self) {
            self.value.set(self.checkpoints.borrow_mut().pop().unwrap());
        }
    }

    #[test]
    fn a_failed_probe_restores_graph_walk_and_semantic_state() {
        let state = SemanticState::default();
        state.value.set(7);
        let stable = SigExprId::from_raw(1);
        let provisional = SigExprId::from_raw(2);
        let mut memo = HashMap::default();
        memo.insert(stable, ResolvedTy::new(Ty::String).unwrap());
        let mut computing = crate::fx_hash::HashSet::from_iter([stable]);

        let result: Result<((), Vec<SigExprId>), ()> =
            transactional(&state, &mut memo, &mut computing, |memo, computing| {
                state.value.set(11);
                memo.insert(provisional, ResolvedTy::new(Ty::Int).unwrap());
                computing.insert(provisional);
                Err(())
            });

        assert_eq!(result, Err(()));
        assert_eq!(state.value.get(), 7);
        assert_eq!(memo.len(), 1);
        assert_eq!(memo.get(&stable).map(|ty| ty.get()), Some(Ty::String));
        assert_eq!(computing, crate::fx_hash::HashSet::from_iter([stable]));
    }
}
