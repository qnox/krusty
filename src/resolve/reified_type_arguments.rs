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
    /// Inferred occurrence path retained from the selected source argument.
    provenance: super::intersection_inference::ReifiedArgumentProvenance,
    /// Complete inference fact retained at selected-call commitment. The ordinary argument stays a
    /// denotable `Ty`; diagnostics must not rebuild this from FIR's additional-bound projection.
    intersection: Option<super::intersection_inference::InferredIntersection>,
}

/// Where and how one type argument is checked.
struct ReifiedArgument {
    span: Span,
    /// Applied source spelling of an explicit argument, parallel to its expanded semantic type.
    spelling: Option<crate::spelling::Spelled>,
    provenance: super::intersection_inference::ReifiedArgumentProvenance,
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
                spelling: written.map(|reference| self.reified_argument_spelling(reference)),
                provenance: written.map_or(reified.provenance, |_| Default::default()),
            };
            let mut problems = Vec::new();
            self.reified_problems(
                scope,
                site.spelling.as_ref(),
                &site.provenance,
                reified.argument,
                reified.intersection.as_ref(),
                false,
                false,
                &mut problems,
            );
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
                intersection: None,
                provenance: self
                    .resolved_call_arg_slots
                    .get(&expression)
                    .and_then(|arguments| arguments.argument_bindings.first())
                    .map(|argument| argument.source_provenance.clone())
                    .unwrap_or_default(),
            }];
        }
        let Some(call) = self.selected_generic_calls.get(&expression) else {
            return Vec::new();
        };
        call.reified_parameters
            .clone()
            .into_iter()
            .filter_map(|(ordinal, formal)| {
                let argument = (*call.arguments().get(ordinal)?)?;
                Some(ReifiedTypeArgument {
                    ordinal,
                    argument,
                    formal,
                    intersection: call.intersections.get(&ordinal).cloned(),
                    provenance: call
                        .argument_provenance(ordinal)
                        .cloned()
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    /// Apply the bind-once alias template selected while this explicit type argument was resolved.
    /// The resulting spelling tree follows the expanded semantic type, while each node remembers
    /// whether `T & Any` was written at this call site rather than inherited from an alias RHS.
    fn reified_argument_spelling(&self, reference: &TypeRef) -> crate::spelling::Spelled {
        let argument_spellings = reference
            .targs
            .iter()
            .map(|argument| self.reified_argument_spelling(argument))
            .collect::<Vec<_>>();
        let selected = self
            .selected_type_alias_applications
            .get(&(reference.span.lo, reference.span.hi));
        let args = selected.map_or_else(
            || argument_spellings.clone(),
            |selected| {
                let use_site = reference
                    .targs
                    .iter()
                    .zip(argument_spellings.iter().cloned())
                    .map(|(argument, spelling)| {
                        let semantic = *self
                            .resolved_type_tys
                            .get(&(argument.span.lo, argument.span.hi))
                            .expect(
                                "a selected type-alias argument has a resolved semantic occurrence",
                            );
                        (semantic, spelling)
                    })
                    .collect::<Vec<_>>();
                super::type_spellings::expansion_arg_spellings(
                    Some((
                        &selected.expansion_spelling,
                        &selected.formals,
                        selected.expansion,
                    )),
                    &use_site,
                )
            },
        );
        crate::spelling::Spelled {
            definitely_non_null: reference.definitely_non_null(),
            use_site_definitely_non_null: reference.definitely_non_null(),
            alias: None,
            alias_args: Vec::new(),
            args,
            annotations: Vec::new(),
            expansion_annotations: Vec::new(),
            unsafe_variance: false,
            parameter_name: None,
            flexible_upper_bound: None,
            enhanced_nullability: false,
        }
    }

    /// kotlinc's `checkArgumentAndReport` over one type argument.
    fn reified_problems(
        &self,
        scope: &CheckerScope<'_>,
        spelling: Option<&crate::spelling::Spelled>,
        provenance: &super::intersection_inference::ReifiedArgumentProvenance,
        argument: Ty,
        intersection: Option<&super::intersection_inference::InferredIntersection>,
        in_array: bool,
        in_dnn_or_flexible: bool,
        problems: &mut Vec<ReifiedProblem>,
    ) {
        let explicitly_dnn = spelling.is_some_and(|spelling| spelling.use_site_definitely_non_null);
        let through_indirect = in_dnn_or_flexible || explicitly_dnn || provenance.is_indirect();
        if explicitly_dnn {
            problems.push(ReifiedProblem::DefinitelyNonNullable);
        }
        if let Some(intersection) = intersection {
            problems.push(ReifiedProblem::Intersection {
                parts: intersection.parts.clone(),
                in_dnn_or_flexible: through_indirect || intersection.through_flexible,
            });
            return;
        }
        if let Some(elements) = array_type_arguments(argument) {
            // A projected element (`out T`, `in T`, `*`) is not materialized.
            for (index, &element) in elements.iter().enumerate() {
                if !matches!(
                    element,
                    Ty::InProjection(_) | Ty::OutProjection(_) | Ty::StarProjection(_)
                ) {
                    self.reified_problems(
                        scope,
                        spelling.map(|spelling| spelling.arg(index)),
                        provenance.argument(index).unwrap_or(
                            super::intersection_inference::ReifiedArgumentProvenance::NONE,
                        ),
                        element,
                        None,
                        true,
                        through_indirect,
                        problems,
                    );
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
                        in_dnn_or_flexible: through_indirect,
                    });
                }
            }
            Ty::Nullable(&Ty::TyParam(name, _)) => {
                if !reified_type_parameter(scope, name) {
                    problems.push(ReifiedProblem::TypeParameter {
                        name,
                        in_array,
                        in_dnn_or_flexible: through_indirect,
                    });
                }
            }
            Ty::DefinitelyNotNull(original) => {
                self.reified_problems(
                    scope, None, provenance, *original, None, in_array, true, problems,
                );
            }
            Ty::Nothing => problems.push(ReifiedProblem::ForbiddenSubstitution("Nothing")),
            Ty::Null | Ty::Nullable(&Ty::Nothing) if !features.nullable_nothing => {
                problems.push(ReifiedProblem::ForbiddenSubstitution("Nothing?"));
            }
            Ty::Intersection(parts) => problems.push(ReifiedProblem::Intersection {
                parts: parts.to_vec(),
                in_dnn_or_flexible: through_indirect,
            }),
            Ty::PlatformNullable(lower) => {
                self.reified_problems(
                    scope, spelling, provenance, *lower, None, in_array, true, problems,
                );
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
                    let warning = features
                        .report_in_dnn_and_flexible_warning_format
                        .as_ref()
                        .map(|format| format.message(&message));
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
                    let warning = features
                        .report_in_dnn_and_flexible_warning_format
                        .as_ref()
                        .map(|format| format.message(&message));
                    self.report_deprecation_error(
                        span,
                        false,
                        warning.as_deref(),
                        crate::diag::CompilerWarningName::TypeIntersectionAsReifiedDeprecation,
                        &message,
                    );
                } else {
                    let warning = features
                        .prohibit_intersection_warning_format
                        .as_ref()
                        .map(|format| format.message(&message));
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
