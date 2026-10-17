//! Reified type arguments: kotlinc's `FirReifiedChecker`.
//!
//! A type argument for a `reified` type parameter, or for the type parameter of `kotlin.Array`'s
//! constructor, is materialized at run time, so it must name a runtime-available type. Each
//! offending argument is reported at its explicit type argument, else at the callee name:
//!
//! - a non-reified type parameter (`T` or `T?`), with its own wording inside an array;
//! - `Nothing`, and `Nothing?` until `NullableNothingInReifiedPosition`;
//! - a definitely-non-nullable type written explicitly;
//! - an inferred intersection, a deprecation warning until `ProhibitIntersectionReifiedTypeParameter`.
//!
//! An `Array<…>` argument is checked through its invariant element types. Reached only through a
//! definitely-non-nullable or flexible type, a type parameter or an intersection is a KTLC-399
//! deprecation warning until `ReportReificationProblemsInDnnAndFlexible`.

use super::*;

/// One type argument the call selected at an expression reifies.
struct ReifiedTypeArgument {
    /// Ordinal among the callee's own type parameters.
    ordinal: usize,
    argument: Ty,
    /// The callee's type-parameter identity, naming the parameter an intersection was inferred for.
    formal: String,
}

/// Where and how one type argument is checked.
struct ReifiedArgument {
    span: Span,
    explicit: bool,
}

/// One finding of the checker, in kotlinc's report order.
enum ReifiedProblem {
    TypeParameter {
        name: &'static str,
        in_array: bool,
        in_dnn_or_flexible: bool,
    },
    ForbiddenSubstitution(&'static str),
    DefinitelyNonNullable,
    Intersection {
        parts: Vec<Ty>,
        in_dnn_or_flexible: bool,
    },
}

impl Checker<'_> {
    /// Check the type arguments `expression`'s selected callee reifies.
    pub(super) fn check_reified_type_arguments(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
    ) {
        if self.discover_anonymous_captures {
            return;
        }
        let arguments = self.reified_type_arguments(expression);
        if arguments.is_empty() {
            return;
        }
        let explicit = self.file.call_type_args.get(&expression.0);
        let mut findings = Vec::new();
        for reified in arguments {
            let written = explicit.and_then(|references| references.get(reified.ordinal));
            let site = ReifiedArgument {
                span: written.map_or_else(|| self.call_callee_name_span(expression), |r| r.span),
                explicit: written.is_some(),
            };
            let mut problems = Vec::new();
            self.reified_problems(scope, &site, reified.argument, false, false, &mut problems);
            findings.push((site.span, reified.formal, problems));
        }
        for (span, formal, problems) in findings {
            for problem in problems {
                self.report_reified_problem(span, &formal, problem);
            }
        }
    }

    /// The type arguments the callee selected at `expression` reifies. A compiler-provided
    /// reference-array creator reifies its element type, the one argument of the `Array` it
    /// returns.
    fn reified_type_arguments(&self, expression: ExprId) -> Vec<ReifiedTypeArgument> {
        if let Some(ExprLowering::CompilerSynthetic(kind)) = self.expr_lowers.get(&expression) {
            let Some(formal) = crate::synthetics::reified_type_parameter(*kind) else {
                return Vec::new();
            };
            let Some(&[element]) = array_type_arguments(self.expr_types[expression.0 as usize])
            else {
                return Vec::new();
            };
            return vec![ReifiedTypeArgument {
                ordinal: 0,
                argument: element,
                formal: formal.to_string(),
            }];
        }
        let Some(arguments) = self.resolved_call_type_args.get(&expression) else {
            return Vec::new();
        };
        let Some(call) = self.resolved_calls.get(&expression) else {
            return Vec::new();
        };
        self.reified_parameters(call, arguments.len())
            .into_iter()
            .filter_map(|(ordinal, formal)| {
                let argument = (*arguments.get(ordinal)?)?;
                Some(ReifiedTypeArgument {
                    ordinal,
                    argument: self.reified_argument_type(expression, ordinal, argument),
                    formal,
                })
            })
            .collect()
    }

    /// The reified type parameters of `call`'s callee, by ordinal among its own type parameters,
    /// with their identities. A declaration of this module answers through its resolved
    /// type-parameter headers; a dependency's callable carries the ordinals it was decoded with.
    fn reified_parameters(&self, call: &ResolvedCall, arity: usize) -> Vec<(usize, String)> {
        let declaration = match call {
            ResolvedCall::Member(selected) => selected.member.stable_declaration,
            ResolvedCall::Companion(member) => member.stable_declaration,
            ResolvedCall::TopLevel(call) => call.stable_declaration,
            ResolvedCall::Extension(call) => call.stable_declaration,
            ResolvedCall::MemberExtension {
                stable_declaration, ..
            } => *stable_declaration,
            // A local function cannot be `inline`, so none of its type parameters is reified.
            ResolvedCall::LocalFunction(_) => return Vec::new(),
        };
        if let Some(declaration) = declaration {
            let Some(index) = self.resolved_index else {
                return Vec::new();
            };
            return (0..arity)
                .filter_map(|ordinal| {
                    let parameter =
                        index.type_parameter(declaration, u32::try_from(ordinal).ok()?)?;
                    if !index.type_parameter_header(parameter)?.flags.is_reified() {
                        return None;
                    }
                    let formal = index.type_parameter_name(parameter)?;
                    Some((ordinal, formal.to_string()))
                })
                .collect();
        }
        let (reified, formals): (&[u32], &[String]) = match call {
            ResolvedCall::Member(selected) => (
                &selected.member.call_sig.reified_type_parameter_ordinals,
                generic_formals(selected.member.generic_sig.as_ref()),
            ),
            ResolvedCall::Companion(member) => (
                &member.call_sig.reified_type_parameter_ordinals,
                generic_formals(member.generic_sig.as_ref()),
            ),
            ResolvedCall::TopLevel(call) => (
                &call.callable.reified_type_parameter_ordinals,
                generic_formals(call.callable.generic_sig.as_deref()),
            ),
            ResolvedCall::Extension(call) => (
                &call.callable.reified_type_parameter_ordinals,
                generic_formals(call.callable.generic_sig.as_deref()),
            ),
            ResolvedCall::MemberExtension {
                reified_type_parameter_ordinals,
                type_parameters,
                ..
            } => (reified_type_parameter_ordinals, type_parameters),
            ResolvedCall::LocalFunction(_) => return Vec::new(),
        };
        reified
            .iter()
            .filter_map(|&ordinal| {
                let ordinal = ordinal as usize;
                Some((ordinal, formals.get(ordinal)?.clone()))
            })
            .collect()
    }

    /// The checked type argument at `ordinal`, as the complete intersection when inference
    /// recorded further constituents beside it.
    fn reified_argument_type(&self, expression: ExprId, ordinal: usize, argument: Ty) -> Ty {
        let bounds = self
            .resolved_call_type_argument_bounds
            .get(&expression)
            .and_then(|bounds| bounds.get(ordinal))
            .filter(|bounds| !bounds.is_empty());
        match bounds {
            Some(bounds) => {
                let mut parts = vec![argument];
                parts.extend(bounds.iter().copied());
                Ty::Intersection(crate::types::intern_tys(&parts))
            }
            None => argument,
        }
    }

    /// kotlinc's `checkArgumentAndReport` over one type argument.
    fn reified_problems(
        &self,
        scope: &CheckerScope<'_>,
        site: &ReifiedArgument,
        argument: Ty,
        in_array: bool,
        in_dnn_or_flexible: bool,
        problems: &mut Vec<ReifiedProblem>,
    ) {
        if let Some(elements) = array_type_arguments(argument) {
            // A projected element (`out T`, `in T`, `*`) is not materialized.
            for &element in elements {
                if !matches!(
                    element,
                    Ty::InProjection(_) | Ty::OutProjection(_) | Ty::StarProjection(_)
                ) {
                    self.reified_problems(scope, site, element, true, false, problems);
                }
            }
            return;
        }
        let features = &self.file.reified_type_arguments;
        match argument {
            Ty::TyParam(name, _) => {
                if !reified_type_parameter(scope, name) {
                    problems.push(ReifiedProblem::TypeParameter {
                        name,
                        in_array,
                        in_dnn_or_flexible,
                    });
                }
            }
            Ty::Nullable(&Ty::TyParam(name, _)) => {
                if !reified_type_parameter(scope, name) {
                    problems.push(ReifiedProblem::TypeParameter {
                        name,
                        in_array,
                        in_dnn_or_flexible,
                    });
                }
            }
            Ty::DefinitelyNotNull(original) => {
                // kotlinc infers some arguments to definitely-non-nullable types; only a written
                // one is reported.
                if site.explicit {
                    problems.push(ReifiedProblem::DefinitelyNonNullable);
                }
                self.reified_problems(scope, site, *original, in_array, true, problems);
            }
            Ty::Nothing => problems.push(ReifiedProblem::ForbiddenSubstitution("Nothing")),
            Ty::Null | Ty::Nullable(&Ty::Nothing) if !features.nullable_nothing => {
                problems.push(ReifiedProblem::ForbiddenSubstitution("Nothing?"));
            }
            Ty::Intersection(parts) => problems.push(ReifiedProblem::Intersection {
                parts: parts.to_vec(),
                in_dnn_or_flexible,
            }),
            Ty::PlatformNullable(lower) => {
                self.reified_problems(scope, site, *lower, in_array, true, problems);
            }
            _ => {}
        }
    }

    fn report_reified_problem(&mut self, span: Span, formal: &str, problem: ReifiedProblem) {
        let features = self.file.reified_type_arguments.clone();
        match problem {
            ReifiedProblem::TypeParameter {
                name,
                in_array,
                in_dnn_or_flexible,
            } => {
                let name = crate::types::type_parameter_source_name(name);
                if in_dnn_or_flexible && !features.report_in_dnn_and_flexible {
                    let message = type_parameter_as_reified(name);
                    let warning = deprecation_warning_message(
                        &message,
                        features
                            .report_in_dnn_and_flexible_warning_suffix
                            .as_deref(),
                    );
                    self.report_deprecation_error(
                        span,
                        false,
                        warning.as_deref(),
                        crate::diag::CompilerWarningName::TypeParameterAsReifiedDeprecation,
                        &message,
                    );
                } else if in_array {
                    self.diags.error(
                        span,
                        format!(
                            "cannot use '{name}' as reified type parameter, since the array type \
                             parameter is not reified."
                        ),
                    );
                } else {
                    self.diags.error(span, type_parameter_as_reified(name));
                }
            }
            ReifiedProblem::ForbiddenSubstitution(rendered) => self.diags.error(
                span,
                format!("cannot use '{rendered}' as reified type parameter."),
            ),
            ReifiedProblem::DefinitelyNonNullable => self.diags.error(
                span,
                "cannot use definitely-non-nullable type as a reified type argument.",
            ),
            ReifiedProblem::Intersection {
                parts,
                in_dnn_or_flexible,
            } => {
                let message = intersection_as_reified(formal, &parts);
                if in_dnn_or_flexible && !features.report_in_dnn_and_flexible {
                    let warning = deprecation_warning_message(
                        &message,
                        features
                            .report_in_dnn_and_flexible_warning_suffix
                            .as_deref(),
                    );
                    self.report_deprecation_error(
                        span,
                        false,
                        warning.as_deref(),
                        crate::diag::CompilerWarningName::TypeIntersectionAsReifiedDeprecation,
                        &message,
                    );
                } else {
                    let warning = deprecation_warning_message(
                        &message,
                        features.prohibit_intersection_warning_suffix.as_deref(),
                    );
                    self.report_deprecation_error(
                        span,
                        features.prohibit_intersection,
                        warning.as_deref(),
                        crate::diag::CompilerWarningName::TypeIntersectionAsReified,
                        &message,
                    );
                }
            }
        }
    }
}

/// Attach a parser-selected deprecation suffix to a message built from resolved types.
fn deprecation_warning_message(message: &str, suffix: Option<&str>) -> Option<String> {
    suffix.map(|suffix| {
        let separator = if message.ends_with('.') || message.ends_with(char::is_whitespace) {
            " "
        } else {
            ". "
        };
        format!("{message}{separator}{suffix}")
    })
}

/// Whether the type parameter with identity `identity`, in scope here, is `reified`.
fn reified_type_parameter(scope: &CheckerScope<'_>, identity: &str) -> bool {
    for rung in scope.classifier_rungs() {
        let mut found = None;
        rung.own_bindings(Ns::Classifier, |_, binding| {
            if let ScopeBinding::TypeParam { bound, reified, .. } = binding {
                if bound.non_null().ty_param_name() == Some(identity) {
                    found = Some(*reified);
                }
            }
        });
        if let Some(reified) = found {
            return reified;
        }
    }
    false
}

fn generic_formals(signature: Option<&crate::libraries::GenericSig>) -> &[String] {
    signature.map_or(&[], |signature| signature.formals.as_slice())
}

/// The type arguments of a fully expanded `kotlin.Array` type, nullable or not.
fn array_type_arguments(ty: Ty) -> Option<&'static [Ty]> {
    match ty {
        Ty::Obj(name, arguments) if name == crate::types::wk::array() => Some(arguments),
        Ty::Nullable(&Ty::Obj(name, arguments)) if name == crate::types::wk::array() => {
            Some(arguments)
        }
        _ => None,
    }
}

fn type_parameter_as_reified(name: &str) -> String {
    format!("cannot use '{name}' as reified type parameter. Use a class instead.")
}

fn intersection_as_reified(formal: &str, parts: &[Ty]) -> String {
    let formal = crate::types::type_parameter_source_name(formal);
    let parts = parts
        .iter()
        .map(|part| {
            format!(
                "'{}'",
                part.source_name_with_type_parameter_in(parts, &|name| {
                    crate::types::type_parameter_source_name(name).to_string()
                })
            )
        })
        .collect::<Vec<_>>()
        .join(" & ");
    format!(
        "type argument for reified type parameter '{formal}' was inferred to the intersection of \
         [{parts}]. Reification of an intersection type results in the common supertype being \
         used. This may lead to subtle issues and an explicit type argument is encouraged."
    )
}
