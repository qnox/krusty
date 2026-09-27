//! Eager lambda analysis (`+EagerLambdaAnalysis`), ported from kotlinc FIR's
//! `EagerLambdaResolution.kt` and the `unitCoercionInLambdas` discrimination of
//! `ConeOverloadConflictResolver`.
//!
//! When several candidates of a call apply and a lambda argument's shape does not tell them apart,
//! kotlinc analyzes that lambda before it compares the candidates' specificity:
//!
//! - Only applicable candidates take part, and only while at least two remain. A candidate with a
//!   contract stops the analysis for the whole call.
//! - Lambdas are taken in source order. A lambda is analyzed when every candidate expects a
//!   function type for it (a functional-interface parameter contributes its method's function type)
//!   with the same, already fixed input types; otherwise the next lambda is considered.
//! - The lambda is analyzed once, with those inputs. Each candidate then receives its results as
//!   constraints on the candidate's expected result: every labelled `return` value, and the last
//!   statement unless that result is `Unit`, which coerces it. A candidate that coerced a result
//!   not already `Unit` is marked as using the coercion. A lambda ending in a statement, and an
//!   empty lambda, result in `Unit`.
//! - Candidates whose constraints fail drop out. When none remain, the first candidate is kept so
//!   that its own diagnostics are reported. The analysis repeats for the next lambda while more
//!   than one candidate remains.
//! - Among the remaining candidates, those that needed no coercion to `Unit` are preferred before
//!   specificity is compared.
//!
//! The analysis narrows the overload family the rest of call resolution sees. The probe's
//! diagnostics are dropped: the lambda is checked again against the candidate finally selected.

use std::collections::HashSet;

use crate::ast::{Expr, ExprId, Stmt};
use crate::libraries::{FnKind, FunctionInfo, LibraryMember};
use crate::types::{ty_mentions_param, FnSig, Ty};

use super::{
    call_argument_parameter_indices, lambda_returns::ReturnTarget, ArgSlots, Checker, CheckerScope,
    LambdaShape,
};

/// The syntax of a call whose candidates eager lambda analysis may narrow.
pub(super) struct EagerLambdaCall<'a> {
    pub(super) shape: super::lambda_call_shapes::UntypedLambdaCall<'a>,
    /// The label the call contributes to its lambdas.
    pub(super) label: Option<&'a str>,
}

/// One applicable candidate taking part in the analysis.
struct EagerCandidate {
    /// The candidate's own type parameters.
    formals: Vec<String>,
    /// Per argument, the function type the candidate expects for a lambda literal.
    lambdas: Vec<Option<&'static FnSig>>,
    has_contract: bool,
    uses_unit_coercion: bool,
}

/// How an analyzed lambda's body ends.
enum LambdaTail {
    /// A last expression, with its type.
    Expression(ExprId, Ty),
    /// An empty body or one ending in a statement: the lambda results in `Unit`.
    Unit,
    /// A last `return`, `break` or `continue`, which results in nothing.
    Jump,
}

/// The results of analyzing a lambda once.
struct LambdaResults {
    tail: LambdaTail,
    /// The type of each value returned through a labelled `return`.
    returns: Vec<Ty>,
}

/// `candidates` without those eliminated: applicable (`Some` in `positions`) but not surviving.
fn without_eliminated<T>(
    candidates: Vec<T>,
    positions: &[Option<usize>],
    survivors: &[usize],
) -> Vec<T> {
    candidates
        .into_iter()
        .zip(positions)
        .filter(|(_, position)| position.is_none_or(|position| survivors.contains(&position)))
        .map(|(candidate, _)| candidate)
        .collect()
}

impl Checker<'_> {
    /// Narrow the receiver-less overload family `family` of a call with lambda arguments. The
    /// family is returned unchanged when the language feature is off or the analysis does not
    /// apply.
    pub(super) fn eager_lambda_family(
        &mut self,
        scope: &CheckerScope<'_>,
        call: EagerLambdaCall<'_>,
        family: Vec<FunctionInfo>,
    ) -> Vec<FunctionInfo> {
        if !self.file.eager_lambda_analysis {
            return family;
        }
        let candidates = family
            .iter()
            .map(|function| self.eager_top_level_candidate(scope, &call, function))
            .collect::<Vec<_>>();
        match self.eager_lambda_survivors(scope, &call, candidates) {
            Some((positions, survivors)) => without_eliminated(family, &positions, &survivors),
            None => family,
        }
    }

    /// Narrow the member overloads `members` of the call `call_expression` with lambda arguments,
    /// and keep what was eliminated for the call's final selection
    /// ([`Self::eager_lambda_retains_member`]).
    pub(super) fn eager_lambda_members(
        &mut self,
        scope: &CheckerScope<'_>,
        call_expression: ExprId,
        call: EagerLambdaCall<'_>,
        members: Vec<LibraryMember>,
    ) -> Vec<LibraryMember> {
        if !self.file.eager_lambda_analysis {
            return members;
        }
        let candidates = members
            .iter()
            .map(|member| self.eager_member_candidate(scope, &call, member))
            .collect::<Vec<_>>();
        let Some((positions, survivors)) = self.eager_lambda_survivors(scope, &call, candidates)
        else {
            return members;
        };
        let eliminated = members
            .iter()
            .zip(&positions)
            .filter(|(_, position)| position.is_some_and(|position| !survivors.contains(&position)))
            .map(|(member, _)| member.params.clone())
            .collect();
        self.eager_eliminated_members
            .insert(call_expression, eliminated);
        without_eliminated(members, &positions, &survivors)
    }

    /// Whether the final selection of `call` keeps `member`: eager lambda analysis did not
    /// eliminate it while its lambdas were planned.
    pub(super) fn eager_lambda_retains_member(&self, call: ExprId, member: &LibraryMember) -> bool {
        self.eager_eliminated_members
            .get(&call)
            .is_none_or(|eliminated| !eliminated.contains(&member.params))
    }

    /// Run the analysis over `candidates` (`None` where a candidate is not applicable). Answers,
    /// when the analysis applied, each candidate's position among the applicable ones and the
    /// positions that survive.
    fn eager_lambda_survivors(
        &mut self,
        scope: &CheckerScope<'_>,
        call: &EagerLambdaCall<'_>,
        candidates: Vec<Option<EagerCandidate>>,
    ) -> Option<(Vec<Option<usize>>, Vec<usize>)> {
        let mut next = 0;
        let positions = candidates
            .iter()
            .map(|candidate| {
                candidate.as_ref().map(|_| {
                    next += 1;
                    next - 1
                })
            })
            .collect::<Vec<_>>();
        let mut candidates = candidates
            .into_iter()
            .flatten()
            .enumerate()
            .collect::<Vec<_>>();
        if candidates.len() < 2
            || candidates
                .iter()
                .any(|(_, candidate)| candidate.has_contract)
        {
            return None;
        }
        let mut analyzed = HashSet::new();
        while candidates.len() > 1 {
            let Some((argument, results)) =
                self.analyze_first_ready_lambda(scope, call, &candidates, &mut analyzed)
            else {
                break;
            };
            let first = candidates[0].0;
            candidates.retain_mut(|(_, candidate)| {
                let Some(expected) = candidate.lambdas[argument] else {
                    return false;
                };
                match self.eager_results_fit(scope, &results, expected.ret, &candidate.formals) {
                    Some(coerced) => {
                        candidate.uses_unit_coercion |= coerced;
                        true
                    }
                    None => false,
                }
            });
            crate::trace_compiler!(
                "resolve",
                "eager lambda argument={argument} survivors={:?}",
                candidates
                    .iter()
                    .map(|(position, _)| *position)
                    .collect::<Vec<_>>(),
            );
            if candidates.is_empty() {
                return Some((positions, vec![first]));
            }
        }
        if candidates
            .iter()
            .any(|(_, candidate)| !candidate.uses_unit_coercion)
        {
            candidates.retain(|(_, candidate)| !candidate.uses_unit_coercion);
        }
        let survivors = candidates.iter().map(|(position, _)| *position).collect();
        Some((positions, survivors))
    }

    /// A receiver-less overload as a candidate, when it takes the call's partially typed
    /// arguments.
    fn eager_top_level_candidate(
        &self,
        scope: &CheckerScope<'_>,
        call: &EagerLambdaCall<'_>,
        function: &FunctionInfo,
    ) -> Option<EagerCandidate> {
        if function.kind != FnKind::TopLevel || !self.source_callable_visible(function) {
            return None;
        }
        let shape = self.top_level_overload_lambda_shape(scope, function, call.shape)?;
        let lambdas = (0..call.shape.args.len())
            .map(|argument| {
                let expected = shape
                    .expected_types
                    .as_ref()
                    .and_then(|types| types.get(argument).copied().flatten())
                    .or_else(|| shape.argument_parameters.get(argument).copied())?;
                self.eager_lambda_expectation(call.shape.args[argument], expected)
            })
            .collect();
        Some(EagerCandidate {
            formals: shape.generic_formals,
            lambdas,
            has_contract: function
                .callable
                .contract
                .as_ref()
                .is_some_and(|contract| !contract.effects.is_empty()),
            uses_unit_coercion: false,
        })
    }

    /// A member overload as a candidate, when it takes the call's partially typed arguments.
    fn eager_member_candidate(
        &self,
        scope: &CheckerScope<'_>,
        call: &EagerLambdaCall<'_>,
        member: &LibraryMember,
    ) -> Option<EagerCandidate> {
        let formals = member
            .generic_sig
            .as_ref()
            .map(|signature| signature.formals.clone())
            .unwrap_or_default();
        if !call.shape.type_args.is_empty() && formals.len() != call.shape.type_args.len() {
            return None;
        }
        let contextual = self.contextual_call_shape(
            scope,
            &member.params,
            &member.call_sig,
            member.context_count,
            call.shape.arg_names,
        )?;
        self.call_candidate_score(
            scope,
            &contextual.params,
            &contextual.call_sig,
            ArgSlots {
                args: call.shape.args,
                partial_arg_tys: call.shape.partial,
                arg_names: call.shape.arg_names,
                trailing_lambda: call.shape.trailing_lambda,
            },
        )?;
        let parameters = call_argument_parameter_indices(
            call.shape.args.len(),
            contextual.params.len(),
            call.shape.arg_names,
            call.shape.trailing_lambda,
            &contextual.call_sig,
        )?;
        let lambdas = call
            .shape
            .args
            .iter()
            .zip(parameters)
            .map(|(&argument, parameter)| {
                let declared = *contextual.params.get(parameter)?;
                let expected = if contextual.call_sig.vararg_index == Some(parameter) {
                    declared.array_read_elem()?
                } else {
                    declared
                };
                self.eager_lambda_expectation(argument, expected)
            })
            .collect();
        Some(EagerCandidate {
            formals,
            lambdas,
            has_contract: member
                .contract
                .as_ref()
                .is_some_and(|contract| !contract.effects.is_empty()),
            uses_unit_coercion: false,
        })
    }

    /// The function type a candidate expecting `expected` gives the argument `argument`, when that
    /// argument is a lambda literal: a function type itself, or the method of a functional
    /// interface.
    fn eager_lambda_expectation(&self, argument: ExprId, expected: Ty) -> Option<&'static FnSig> {
        if !matches!(self.file.expr(argument), Expr::Lambda { .. })
            || self.file.anon_fun_lambdas.contains(&argument.0)
        {
            return None;
        }
        let function = match expected.non_null() {
            Ty::Fun(function) => return Some(function),
            interface => self.semantic_sam_signature(interface)?,
        };
        match Ty::fun_with_shape(
            function.params,
            function.ret,
            function.context_count,
            function.has_receiver,
            function.suspend,
        ) {
            Ty::Fun(function) => Some(function),
            _ => None,
        }
    }

    /// Analyze the first lambda, in source order, that every candidate expects as a function
    /// with the same fixed inputs.
    fn analyze_first_ready_lambda(
        &mut self,
        scope: &CheckerScope<'_>,
        call: &EagerLambdaCall<'_>,
        candidates: &[(usize, EagerCandidate)],
        analyzed: &mut HashSet<usize>,
    ) -> Option<(usize, LambdaResults)> {
        for argument in 0..call.shape.args.len() {
            if analyzed.contains(&argument) {
                continue;
            }
            let Some(expectations) = candidates
                .iter()
                .map(|(_, candidate)| candidate.lambdas[argument])
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let inputs = expectations[0];
            let same_inputs = expectations.iter().all(|expected| {
                expected.params == inputs.params
                    && expected.context_count == inputs.context_count
                    && expected.has_receiver == inputs.has_receiver
            });
            let fixed_inputs = candidates.iter().all(|(_, candidate)| {
                inputs.params.iter().all(|&input| {
                    !input.mentions_error()
                        && !ty_mentions_param(input, &candidate.formals)
                        && Self::type_is_lexically_fixed(scope, input)
                })
            });
            if !same_inputs || !fixed_inputs {
                continue;
            }
            analyzed.insert(argument);
            let results =
                self.analyze_lambda_eagerly(scope, call.shape.args[argument], inputs, call.label);
            return Some((argument, results));
        }
        None
    }

    /// Check `lambda` with the inputs of `inputs` and no expected result, and read its results. The
    /// check is a probe: its diagnostics are dropped.
    fn analyze_lambda_eagerly(
        &mut self,
        scope: &CheckerScope<'_>,
        lambda: ExprId,
        inputs: &'static FnSig,
        label: Option<&str>,
    ) -> LambdaResults {
        let context_count = inputs.context_count.min(inputs.params.len());
        let value_start = context_count + usize::from(inputs.has_receiver);
        let checkpoint = self.diags.diags.len();
        self.lambda_returns.collect_returns(lambda);
        self.check_lambda_with_implicit_receivers_labeled(
            scope,
            lambda,
            LambdaShape {
                context_types: &inputs.params[..context_count],
                extension_receiver: inputs
                    .has_receiver
                    .then(|| inputs.params.get(context_count).copied())
                    .flatten(),
                value_types: inputs.params.get(value_start..).unwrap_or_default(),
            },
            inputs.suspend,
            label,
        );
        let mut returns = self.lambda_returns.take_collected_returns();
        self.diags.diags.truncate(checkpoint);
        // A labelled `return` without a value, in expression position, returns `Unit`.
        returns.extend(
            self.expr_return_targets
                .iter()
                .filter(|&(&expression, &target)| {
                    target == ReturnTarget::Lambda(lambda)
                        && matches!(self.file.expr(expression), Expr::Return { value: None, .. })
                })
                .map(|_| Ty::Unit),
        );
        let Expr::Lambda { body, .. } = self.file.expr(lambda) else {
            unreachable!("eager analysis checks lambda literals")
        };
        let last = match self.file.expr(*body) {
            Expr::Block {
                trailing: Some(last),
                ..
            } => Some(*last),
            Expr::Block {
                stmts,
                trailing: None,
            } => match stmts.last().map(|&statement| self.file.stmt(statement)) {
                Some(Stmt::Expr(last)) => Some(*last),
                Some(Stmt::Return(..) | Stmt::Break(_) | Stmt::Continue(_)) => {
                    return LambdaResults {
                        tail: LambdaTail::Jump,
                        returns,
                    };
                }
                _ => None,
            },
            _ => Some(*body),
        };
        let tail = last.map_or(LambdaTail::Unit, |last| {
            LambdaTail::Expression(last, self.expr_types[last.0 as usize])
        });
        LambdaResults { tail, returns }
    }

    /// Whether a candidate expecting the lambda to result in `expected` accepts `results`, and if
    /// so whether it coerced the last expression to `Unit`. An expected result that mentions one of
    /// the candidate's own `formals` takes the results as constraints still to be solved.
    fn eager_results_fit(
        &self,
        scope: &CheckerScope<'_>,
        results: &LambdaResults,
        expected: Ty,
        formals: &[String],
    ) -> Option<bool> {
        if ty_mentions_param(expected, formals) {
            return Some(false);
        }
        let fits =
            |actual: Ty| actual.mentions_error() || self.receiver_is_assignable(actual, expected);
        if !results.returns.iter().all(|&returned| fits(returned)) {
            return None;
        }
        match results.tail {
            LambdaTail::Unit => fits(Ty::Unit).then_some(false),
            LambdaTail::Jump => Some(false),
            LambdaTail::Expression(_, actual) if expected == Ty::Unit => {
                Some(!actual.mentions_error() && !self.receiver_is_assignable(actual, Ty::Unit))
            }
            LambdaTail::Expression(last, actual) => {
                fits(self.expression_type_for_expected(scope, last, actual, expected))
                    .then_some(false)
            }
        }
    }
}
