//! Why a source classifier's constructor call failed: the argument-mapping reason every candidate
//! agrees on, and the too-many-arguments report every constructor origin shares.

use super::*;

/// The value-parameter list a constructor-arity diagnostic is written against.
pub(super) struct ConstructorValueParameters<'a> {
    pub(super) types: &'a [Ty],
    pub(super) names: &'a [String],
    pub(super) defaults: &'a [bool],
    pub(super) vararg: Option<usize>,
}

impl Checker<'_> {
    /// Report WHY no source constructor accepted the call, when every candidate agrees on the reason.
    ///
    /// `select_source_constructor` answers only yes/no, so the reason was dropped and the caller fell back
    /// to the generic "none of the following candidates is applicable:" list — `Point()` on
    /// `class Point(val x: Int)` said that instead of "no value passed for parameter 'x'.", which is what
    /// every other callable origin reports for the same mistake. Re-run the argument mapping over the
    /// candidates to recover it. Returns whether a canonical error was reported.
    pub(super) fn report_source_constructor_mapping_error(
        &mut self,
        call: ExprId,
        args: &[ExprId],
        arguments: &CtorDelegationCall,
        candidates: &[CtorDelegationCandidate],
        class_name: &str,
    ) -> bool {
        let names: Option<&[Option<String>]> = arguments
            .names
            .iter()
            .any(Option::is_some)
            .then_some(&arguments.names);
        let trailing_lambda = self.file.call_has_trailing_lambda.contains(&call.0);
        let mut failures = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            let params = crate::libraries::ParamList {
                visibility: Visibility::Public,
                names: candidate.param_names.clone(),
                defaults: candidate.defaults.as_slice().into(),
                types: candidate.target.params().to_vec(),
                recv_fun: candidate.lambda_receivers.as_slice().into(),
                vararg: candidate.vararg,
                annotation: None,
            };
            if let Err(failure) = map_param_list_args(args, names, &params, trailing_lambda) {
                failures.push((failure, index));
            }
        }
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

    pub(super) fn report_constructor_too_many_arguments(
        &mut self,
        call: ExprId,
        args: &[ExprId],
        failure: &CallArgMappingFailure,
        class_name: &str,
        parameters: ConstructorValueParameters<'_>,
    ) -> bool {
        let ConstructorValueParameters {
            types: params,
            names,
            defaults,
            vararg,
        } = parameters;
        if !matches!(
            failure.errors.as_slice(),
            [CallArgMappingError::TooManyArguments { .. }]
        ) || names.len() != params.len()
        {
            return false;
        }
        let rendered = names
            .iter()
            .zip(params)
            .enumerate()
            .map(|(index, (name, ty))| {
                let default = if defaults.get(index).copied().unwrap_or(false) {
                    " = ..."
                } else {
                    ""
                };
                let vararg = if vararg == Some(index) { "vararg " } else { "" };
                format!("{vararg}{name}: {}{default}", ty.source_name())
            })
            .collect::<Vec<_>>()
            .join(", ");
        let span = self.arity_diagnostic_span(call, args, names.len());
        self.diags.error(
            span,
            format!("too many arguments for 'constructor({rendered}): {class_name}'."),
        );
        true
    }
}
