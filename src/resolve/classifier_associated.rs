//! Calls, property accesses and callable references answered by classifier-associated
//! declarations: `companion { … }` block members and written `companion fun/val C.name`.
//!
//! Providers publish these declarations as receiver-less candidates of their classifier's
//! namespace (see `symbol_resolver::classifier_associated`). They are named through the classifier
//! coordinate: qualified (`C.name`), or unqualified from `C`'s static scope, which a class body and
//! a companion-associated declaration open. Selection is the ordinary receiver-less one — the shared
//! argument mapper and overload selector followed by the top-level call record — so an associated
//! call has no runtime receiver at any later phase.

use super::*;

/// The outcome of one associated rung for a call.
pub(super) enum AssociatedCall {
    /// The rung declares no associated function with this name.
    Absent,
    /// Candidates exist but none applies; they join the caller's inapplicability report.
    Inapplicable(Vec<crate::libraries::FunctionInfo>),
    /// A candidate was selected (or an ambiguity reported) and the call recorded: its result type.
    Selected(Ty),
}

impl AssociatedCall {
    /// The selected call's result type; inapplicable candidates join `inapplicable`.
    fn selected_or_report(
        self,
        inapplicable: &mut Vec<crate::libraries::FunctionInfo>,
    ) -> Option<Ty> {
        match self {
            AssociatedCall::Selected(ret) => Some(ret),
            AssociatedCall::Inapplicable(candidates) => {
                inapplicable.extend(candidates);
                None
            }
            AssociatedCall::Absent => None,
        }
    }
}

/// An unqualified call's arguments and constraints, shared by every associated rung it tries.
#[derive(Clone, Copy)]
pub(super) struct AssociatedCallSite<'a> {
    pub(super) call_args: CallArgs<'a>,
    pub(super) argument_names: Option<&'a [Option<String>]>,
    pub(super) explicit_type_args: &'a [Ty],
    pub(super) expected: Option<Ty>,
}

impl Checker<'_> {
    /// Classifiers whose static scope is lexically open here, innermost first: the classifier of an
    /// enclosing companion-associated declaration, then the enclosing classes.
    pub(super) fn static_scope_classifiers(&self, scope: &CheckerScope<'_>) -> Vec<TypeName> {
        let mut classifiers = scope.companion_classifiers();
        for classifier in self.lexical_source_class_names() {
            if !classifiers.contains(&classifier) {
                classifiers.push(classifier);
            }
        }
        classifiers
    }

    /// Select `name(args)` among one rung's associated `candidates` and record the selected callable
    /// as a receiver-less call. `probe_mark` is the argument probe to retire once a candidate is
    /// selected, when the caller typed the arguments through one.
    pub(super) fn associated_call(
        &mut self,
        scope: &CheckerScope<'_>,
        CallArgs {
            call,
            args,
            arg_tys,
        }: CallArgs<'_>,
        (name, argument_names): (&str, Option<&[Option<String>]>),
        candidates: Vec<crate::libraries::FunctionInfo>,
        explicit_type_args: &[Ty],
        (expected, probe_mark): (Option<Ty>, Option<usize>),
    ) -> AssociatedCall {
        if candidates.is_empty() {
            return AssociatedCall::Absent;
        }
        let selection = self.select_callable_candidate(
            scope,
            CallArgs {
                call,
                args,
                arg_tys,
            },
            explicit_type_args,
            None,
            CallResultConstraint::direct(expected),
            candidates.clone(),
        );
        match selection {
            Some(
                CallableCandidateSelection::Selected(selected)
                | CallableCandidateSelection::MissingContext(selected),
            ) => {
                if let Some(probe_mark) = probe_mark {
                    self.retire_selected_lambda_probe(call, args, probe_mark);
                }
                AssociatedCall::Selected(self.finish_top_level_call(
                    scope,
                    call,
                    args,
                    arg_tys,
                    argument_names,
                    *selected,
                    explicit_type_args,
                    None,
                ))
            }
            Some(CallableCandidateSelection::Ambiguous(ambiguous)) => {
                self.report_callable_ambiguity(call, name, &ambiguous);
                AssociatedCall::Selected(Ty::Error)
            }
            None => AssociatedCall::Inapplicable(candidates),
        }
    }

    /// `C.name(args)` naming `classifier`'s associated functions. These precede the members of
    /// `C`'s companion-object value, so this rung runs before `C` is read as a value; `None` when
    /// `classifier` declares no associated function named `name`.
    pub(super) fn qualified_associated_call(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        args: &[ExprId],
        argument_names: Option<&[Option<String>]>,
        (classifier, name): (TypeName, &str),
        expected: Option<Ty>,
    ) -> Option<Ty> {
        let candidates = self
            .resolver()
            .classifier_associated_callables(classifier, name);
        if candidates.is_empty() {
            return None;
        }
        let (arg_tys, probe_mark) = self.probe_argument_types(scope, call, args);
        let explicit_type_args = self.explicit_call_type_args(scope, call);
        let candidates = match self.associated_call(
            scope,
            CallArgs {
                call,
                args,
                arg_tys: &arg_tys,
            },
            (name, argument_names),
            candidates,
            &explicit_type_args,
            (expected, Some(probe_mark)),
        ) {
            AssociatedCall::Selected(ret) => return Some(ret),
            AssociatedCall::Inapplicable(candidates) => candidates,
            AssociatedCall::Absent => return None,
        };
        let reported = self.report_inapplicable_callable_candidates(
            InapplicableTopLevelCall {
                call,
                name,
                args,
                argument_names,
                trailing_lambda: self.file.call_has_trailing_lambda.contains(&call.0),
                mapping_error_reported: false,
                explicit_type_args,
            },
            candidates,
        );
        if !reported {
            self.diags.error(
                self.call_callee_name_span(call),
                INAPPLICABLE_OVERLOAD_PREFIX.to_string(),
            );
        }
        Some(Ty::Error)
    }

    /// The associated rung of `classifier`'s static scope for an unqualified call: the selected
    /// call's result type, or `None` with inapplicable candidates added to `inapplicable`.
    pub(super) fn static_scope_call(
        &mut self,
        scope: &CheckerScope<'_>,
        site: AssociatedCallSite<'_>,
        (name, classifier): (&str, TypeName),
        inapplicable: &mut Vec<crate::libraries::FunctionInfo>,
    ) -> Option<Ty> {
        let candidates = self
            .resolver()
            .static_scope_associated_callables(classifier, name);
        self.associated_call(
            scope,
            site.call_args,
            (name, site.argument_names),
            candidates,
            site.explicit_type_args,
            (site.expected, None),
        )
        .selected_or_report(inapplicable)
    }

    /// The associated `operator fun invoke` a bare classifier call `C(args)` selects before the
    /// classifier's runtime value: the selected call's result type, or `None` with inapplicable
    /// candidates added to `inapplicable`.
    pub(super) fn classifier_invoke_call(
        &mut self,
        scope: &CheckerScope<'_>,
        site: AssociatedCallSite<'_>,
        classifier: TypeName,
        inapplicable: &mut Vec<crate::libraries::FunctionInfo>,
    ) -> Option<Ty> {
        let candidates = self
            .resolver()
            .classifier_associated_callables(classifier, CALLABLE_INVOKE_OPERATOR);
        self.associated_call(
            scope,
            site.call_args,
            (CALLABLE_INVOKE_OPERATOR, site.argument_names),
            candidates,
            site.explicit_type_args,
            (site.expected, None),
        )
        .selected_or_report(inapplicable)
    }

    /// The associated property `name` of `classifier`'s static scope: the nearest classifier
    /// declaring one, with its context arguments selected.
    pub(super) fn select_static_scope_property(
        &self,
        scope: &CheckerScope<'_>,
        classifier: TypeName,
        name: &str,
    ) -> TopLevelPropertySelection {
        let mut properties = self
            .resolver()
            .static_scope_associated_properties(classifier, name);
        if let Some(nearest) = properties
            .iter()
            .map(|property| property.receiver_rank)
            .min()
        {
            properties.retain(|property| property.receiver_rank == nearest);
        }
        self.select_top_level_property_candidates(scope, properties)
    }

    /// The associated property `name` of the nearest lexically open static scope declaring one.
    pub(super) fn select_scoped_associated_property(
        &self,
        scope: &CheckerScope<'_>,
        name: &str,
    ) -> TopLevelPropertySelection {
        for classifier in self.static_scope_classifiers(scope) {
            let selection = self.select_static_scope_property(scope, classifier, name);
            if !matches!(selection, TopLevelPropertySelection::None) {
                return selection;
            }
        }
        TopLevelPropertySelection::None
    }

    /// Report an associated property the access site cannot see. A provider labels a platform
    /// declaration (a Java static field); a Kotlin companion declaration is labelled as kotlinc
    /// renders it, `companion val name: Type` for a block member of `C` and `companion val C.name:
    /// Type` for a written one, which belongs to its file.
    pub(super) fn report_inaccessible_associated_property(
        &mut self,
        property: &crate::libraries::PropertyInfo,
        span: Span,
    ) {
        let provider_label = property
            .getter
            .external_property_identity
            .and_then(|identity| {
                self.libraries.external_property_diagnostic_label(
                    identity,
                    &property.name,
                    property.ty,
                )
            });
        let Some(classifier) = property
            .associated_classifier
            .filter(|_| provider_label.is_none() && !self.visibility_access_suppressed())
        else {
            self.report_inaccessible_property(
                property.visibility,
                &property.name,
                property.ty,
                property.owner,
                property.getter.external_property_identity,
                span,
            );
            return;
        };
        let written = property.owner != classifier;
        let kind = if property.setter.is_some() {
            "var"
        } else {
            "val"
        };
        let receiver = if written {
            format!("{}.", Ty::obj_name(classifier).source_name())
        } else {
            String::new()
        };
        let container = if written {
            "file".to_string()
        } else {
            format!("'{}'", Self::access_owner_display(classifier))
        };
        let visibility = match property.visibility {
            Visibility::Private => "private",
            Visibility::Protected => "protected",
            Visibility::Internal => "internal",
            Visibility::PackagePrivate => "package-private",
            Visibility::Public => "public",
        };
        self.diags.error(
            span,
            format!(
                "cannot access 'companion {kind} {receiver}{}: {}': it is {visibility} in {container}.",
                property.name,
                property.ty.source_name(),
            ),
        );
    }

    /// `C.name` naming `classifier`'s associated property.
    pub(super) fn select_qualified_associated_property(
        &self,
        scope: &CheckerScope<'_>,
        classifier: TypeName,
        name: &str,
    ) -> TopLevelPropertySelection {
        let properties = self
            .resolver()
            .classifier_associated_properties(classifier, name);
        self.select_top_level_property_candidates(scope, properties)
    }

    /// `C.name = value` naming `classifier`'s associated property: the receiver-less write the
    /// selection denotes, recorded exactly like a top-level property write.
    pub(super) fn write_receiverless_property(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
        value: ExprId,
        name: &str,
        selection: TopLevelPropertySelection,
    ) {
        let target_span = self.assignment_target_span(statement);
        match selection {
            TopLevelPropertySelection::Selected(property) => {
                if property.property.setter.is_none() {
                    self.report_val_reassignment(target_span, "'val' cannot be reassigned.");
                }
                let value_ty = self.expr_expected(scope, value, property.property.ty);
                self.expect_assignable(
                    property.property.ty,
                    value_ty,
                    self.value_diagnostic_span(value, value_ty),
                    "assignment",
                );
                self.stmt_lowers
                    .insert(statement, StmtLowering::TopLevelPropertySet(property));
            }
            TopLevelPropertySelection::Ambiguous => self.diags.error(
                target_span,
                format!("overload resolution ambiguity for property '{name}'"),
            ),
            TopLevelPropertySelection::MissingContext(..) => self.diags.error(
                target_span,
                format!("No context argument for '{name}' found."),
            ),
            TopLevelPropertySelection::None => {}
        }
    }

    /// `C::name`, or `::name` from `C`'s static scope, naming one of `classifier`'s associated
    /// declarations: the same receiver-less reference as `::topLevel`. `None` when no associated
    /// declaration named `name` is reachable.
    pub(super) fn associated_callable_ref(
        &mut self,
        expression: ExprId,
        name: &str,
        (functions, properties): (
            Vec<crate::libraries::FunctionInfo>,
            Vec<crate::libraries::PropertyInfo>,
        ),
        expected: Option<Ty>,
    ) -> Option<Ty> {
        if !functions.is_empty() {
            if let Some(Ty::Fun(expected)) = expected {
                return self
                    .selected_receiverless_function_ref(expression, name, expected, functions);
            }
            let nearest = functions
                .iter()
                .map(|function| function.receiver_rank)
                .min();
            let functions = functions
                .into_iter()
                .filter(|function| Some(function.receiver_rank) == nearest)
                .collect::<Vec<_>>();
            return Some(match functions.as_slice() {
                [selected] => {
                    let ty = if selected.callable.suspend {
                        Ty::fun_suspend(selected.callable.params.clone(), selected.callable.ret)
                    } else {
                        Ty::fun(selected.callable.params.clone(), selected.callable.ret)
                    };
                    self.record_top_level_function_ref(expression, name, selected, ty)
                }
                _ => {
                    self.diags.error(
                        self.member_name_span(expression, name),
                        format!("overload resolution ambiguity for callable reference '{name}'"),
                    );
                    Ty::Error
                }
            });
        }
        let nearest = properties
            .iter()
            .map(|property| property.receiver_rank)
            .min()?;
        let property = properties
            .into_iter()
            .find(|property| property.receiver_rank == nearest && property.context_count == 0)?;
        let ty = self.property_ref_ty(0, property.setter.is_some(), &[property.ty])?;
        self.expr_lowers.insert(
            expression,
            ExprLowering::CallableReference {
                binding: CallableReferenceBinding::Bound,
                target: CallableReferenceTarget::TopLevelProperty(Box::new(property)),
            },
        );
        Some(ty)
    }
}
