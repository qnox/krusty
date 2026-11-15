//! Which rejected family owns a receiver-less call's terminal diagnostic when a classifier's
//! constructors and same-named callables all reject the arguments.
//!
//! kotlinc reports the candidates of the most applicable failure: a candidate whose arguments map
//! onto its parameters but do not fit their types outranks one the arguments cannot even be mapped
//! onto. `Box<String>(3)` against `class Box<T>(val a: Array<T>)` and a factory
//! `fun <T> Box(size: Int, init: (Int) -> T)` therefore reports the constructor's argument type
//! mismatch, not the factory's missing `init`.

use super::super::*;

/// The rejected constructors of the called classifier, from either origin.
pub(in crate::resolve) struct RejectedConstructors<'a> {
    pub(in crate::resolve) source: Option<(&'a CtorDelegationCall, &'a [CtorDelegationCandidate])>,
    pub(in crate::resolve) provider: Option<&'a LibraryConstructorFailure>,
}

impl Checker<'_> {
    /// Whether the rejected constructors own the diagnostic over the rejected callables: some
    /// constructor maps the arguments while no callable does.
    pub(in crate::resolve) fn rejected_constructors_outrank_callables(
        &self,
        call: ExprId,
        args: &[ExprId],
        callables: &[crate::libraries::FunctionInfo],
        constructors: RejectedConstructors<'_>,
    ) -> bool {
        let constructor_maps = constructors.source.is_some_and(|(arguments, candidates)| {
            self.source_constructor_maps_arguments(call, args, arguments, candidates)
        }) || constructors
            .provider
            .is_some_and(|failure| !matches!(failure, LibraryConstructorFailure::Mapping(_)));
        constructor_maps && !self.callable_maps_arguments(call, args, callables)
    }

    fn callable_maps_arguments(
        &self,
        call: ExprId,
        args: &[ExprId],
        candidates: &[crate::libraries::FunctionInfo],
    ) -> bool {
        let names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        candidates.iter().any(|candidate| {
            let context_count = candidate.context_count.min(candidate.callable.params.len());
            map_call_sig_args_with_trailing(
                args,
                names,
                candidate
                    .semantic_signature()
                    .params
                    .len()
                    .saturating_sub(context_count),
                &candidate.call_sig.suffix(context_count),
                trailing_lambda,
            )
            .is_ok()
        })
    }

    fn source_constructor_mapping_failures(
        &self,
        call: ExprId,
        args: &[ExprId],
        arguments: &CtorDelegationCall,
        candidates: &[CtorDelegationCandidate],
    ) -> Vec<(CallArgMappingFailure, usize)> {
        let names: Option<&[Option<String>]> = arguments
            .names
            .iter()
            .any(Option::is_some)
            .then_some(&arguments.names);
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                let params = crate::libraries::ParamList {
                    visibility: Visibility::Public,
                    names: candidate.param_names.clone(),
                    defaults: candidate.defaults.as_slice().into(),
                    types: candidate.target.params().to_vec(),
                    recv_fun: candidate.lambda_receivers.as_slice().into(),
                    vararg: candidate.vararg,
                    annotation: None,
                };
                map_param_list_args(args, names, &params, trailing_lambda)
                    .err()
                    .map(|failure| (failure, index))
            })
            .collect()
    }

    fn source_constructor_maps_arguments(
        &self,
        call: ExprId,
        args: &[ExprId],
        arguments: &CtorDelegationCall,
        candidates: &[CtorDelegationCandidate],
    ) -> bool {
        self.source_constructor_mapping_failures(call, args, arguments, candidates)
            .len()
            < candidates.len()
    }

    /// Report WHY no source constructor accepted the call, when every candidate agrees on the reason.
    ///
    /// `select_source_constructor` answers only yes/no, so the reason was dropped and the caller fell back
    /// to the generic "none of the following candidates is applicable:" list — `Point()` on
    /// `class Point(val x: Int)` said that instead of "no value passed for parameter 'x'.", which is what
    /// every other callable origin reports for the same mistake. Re-run the argument mapping over the
    /// candidates to recover it. Returns whether a canonical error was reported.
    pub(in crate::resolve) fn report_source_constructor_mapping_error(
        &mut self,
        call: ExprId,
        args: &[ExprId],
        arguments: &CtorDelegationCall,
        candidates: &[CtorDelegationCandidate],
        class_name: &str,
    ) -> bool {
        let mut failures =
            self.source_constructor_mapping_failures(call, args, arguments, candidates);
        match take_unanimous_mapping_error(&mut failures) {
            Some((failure, index)) => {
                if let Some(candidate) = candidates.get(index) {
                    if self.report_constructor_too_many_arguments(
                        call,
                        args,
                        &failure,
                        class_name,
                        ConstructorValueParameters {
                            types: candidate.target.params(),
                            names: &candidate.param_names,
                            defaults: &candidate.defaults,
                            vararg: candidate.vararg,
                        },
                    ) {
                        return true;
                    }
                }
                self.report_call_arg_mapping_error(call, args, failure);
                true
            }
            None => false,
        }
    }
}
