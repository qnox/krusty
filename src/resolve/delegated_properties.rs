//! Delegated-property convention selection and checked semantic recording.
//!
//! This module owns the transition from one checked delegate expression to the exact
//! `provideDelegate`/`getValue`/`setValue` declarations consumed by FIR. It reuses ordinary
//! receiver callable collection, member-extension selection, applicability, and generic inference.

use super::*;

enum DelegateGetValueAttempt {
    Complete(Option<Ty>),
    RetryWithReceiver(Ty),
}

#[derive(Clone, Copy)]
struct DelegateGetValueSelection {
    provide_ref: Ty,
    this_ref: Ty,
    expected_property: Option<Ty>,
    site: DelegateConventionSite,
}

/// What kotlinc's delegate-convention diagnostics name beyond the types already in hand: where the
/// `by` keyword sits, which receivers the property has, and whether it is a `var`. The receivers are
/// listed dispatch first — the order `KProperty2<D, E, V>` spells them.
#[derive(Clone, Copy)]
pub(crate) struct DelegateConventionSite {
    pub(crate) by_span: Span,
    pub(crate) dispatch_receiver: Option<Ty>,
    pub(crate) extension_receiver: Option<Ty>,
    pub(crate) is_var: bool,
}

impl DelegateConventionSite {
    /// A DECLARED property's site: the receivers it was resolved with, dispatch first.
    pub(crate) fn of(
        property: &crate::ast::PropDecl,
        dispatch_receiver: Option<Ty>,
        extension_receiver: Option<Ty>,
    ) -> Self {
        Self {
            by_span: property
                .delegate_by_span
                .expect("a delegated property was parsed from its `by` keyword"),
            dispatch_receiver,
            extension_receiver,
            is_var: property.is_var,
        }
    }

    /// A LOCAL delegated property's site. It has no receiver of any kind: its accessors pass `null`
    /// as `thisRef` and a `KProperty0` reference.
    pub(crate) fn local(by_span: Span, is_var: bool) -> Self {
        Self {
            by_span,
            dispatch_receiver: None,
            extension_receiver: None,
            is_var,
        }
    }

    fn receivers(self) -> Vec<Ty> {
        self.dispatch_receiver
            .into_iter()
            .chain(self.extension_receiver)
            .collect()
    }

    fn flavour(self) -> String {
        format!(
            "{}{}",
            if self.is_var {
                "KMutableProperty"
            } else {
                "KProperty"
            },
            self.receivers().len(),
        )
    }

    /// `KMutableProperty1<*, *>` — every argument star-projected. This is the spelling kotlinc uses
    /// whenever it has no single candidate to blame: the property reference it would have passed was
    /// never built, so its arguments are unknown here too.
    fn star_projected_reference(self) -> String {
        let arguments = vec!["*"; self.receivers().len() + 1].join(", ");
        format!("{}<{arguments}>", self.flavour())
    }

    /// `KMutableProperty1<Holder, Long>` — the reference the accessors would really pass: this
    /// property's receivers, then its type.
    fn applied_reference(self, property: Ty) -> String {
        let arguments = self
            .receivers()
            .into_iter()
            .chain(std::iter::once(property))
            .map(delegate_diagnostic_ty)
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}<{arguments}>", self.flavour())
    }
}

/// Why no convention was selected, in the two shapes kotlinc reports.
enum DelegateConventionFailure {
    /// Nothing of that name is visible on the delegate at all.
    Missing,
    /// Functions of that name exist and none is applicable. Carries them rendered as kotlinc lists
    /// them, in the order the delegate's scope offers them.
    NoneApplicable(Vec<String>),
}

/// Render a type the way a delegate diagnostic names it. The literal null type prints as `Nothing?`,
/// which is what kotlinc calls the `thisRef` a receiverless property passes.
fn delegate_diagnostic_ty(ty: Ty) -> String {
    if ty == Ty::Null {
        return "Nothing?".to_string();
    }
    ty.source_name()
}

pub(super) fn select_delegate_operator_return(
    resolver: &crate::symbol_resolver::SymbolResolver,
    receiver: Ty,
    name: &str,
    args: &[Ty],
) -> Option<Ty> {
    match select_delegate_operator(resolver, receiver, name, args) {
        crate::symbol_resolver::CandidateSelection::Selected((_, ret)) => Some(ret),
        crate::symbol_resolver::CandidateSelection::None
        | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
    }
}

#[derive(Clone, Debug)]
pub enum DelegateGetValueTarget {
    Member {
        applied_receiver: Ty,
        declared_receiver: Ty,
        /// The value parameters as the DECLARATION spells them — a type parameter stays a type
        /// parameter. Not [`Self::Member::physical_params`], which is the ABI slot list after
        /// erasure: a consumer that must know a value crosses into a `T` slot needs `T`, because
        /// what `T` costs physically is a target's answer and differs between targets.
        declared_params: Vec<Ty>,
        declared_ret: Ty,
        stable_declaration: Option<crate::fir::DeclarationId>,
        external_identity: Option<crate::fir::ExternalCallableId>,
        external_default_provider: Option<crate::fir::ExternalCallableId>,
        owner: TypeName,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
        physical_params: Vec<Ty>,
        physical_ret: Ty,
        descriptor: String,
        interface: bool,
    },
    Extension {
        callable: Box<crate::libraries::LibraryCallable>,
        stable_declaration: Option<crate::fir::DeclarationId>,
    },
    MemberExtension {
        stable_declaration: Option<crate::fir::DeclarationId>,
        external_identity: Option<crate::fir::ExternalCallableId>,
        external_default_provider: Option<crate::fir::ExternalCallableId>,
        owner: TypeName,
        name: String,
        extension_receiver: Ty,
        dispatch_receiver: ImplicitReceiverSelection,
        context_count: usize,
        params: Vec<Ty>,
        ret: Ty,
        physical_params: Vec<Ty>,
        physical_ret: Ty,
        inline: InlineKind,
        inline_body_plan: Option<Box<crate::libraries::InlineBodyPlan>>,
        suspend: bool,
        /// See [`Self::Member::declared_params`].
        declared_params: Vec<Ty>,
        declared_ret: Option<Ty>,
        interface: bool,
    },
}

impl DelegateGetValueTarget {
    pub fn ret(&self) -> Ty {
        match self {
            Self::Member { ret, .. } => *ret,
            Self::Extension { callable, .. } => callable.ret,
            Self::MemberExtension { ret, .. } => *ret,
        }
    }

    fn applied_receiver(&self) -> Option<Ty> {
        match self {
            Self::Member {
                applied_receiver, ..
            } => Some(*applied_receiver),
            Self::Extension { callable, .. } => callable.source_receiver,
            Self::MemberExtension {
                extension_receiver, ..
            } => Some(*extension_receiver),
        }
    }

    fn receiver_constrained_by_result(&self, expected: Ty) -> Option<Ty> {
        let Self::Member {
            declared_receiver,
            declared_ret,
            ..
        } = self
        else {
            return None;
        };
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        crate::symbol_resolver::unify_inferred_ty(*declared_ret, expected, &mut bindings);
        (!bindings.is_empty())
            .then(|| crate::symbol_resolver::ty_subst_keep_unbound(*declared_receiver, &bindings))
    }
}

/// The names of the delegated-property conventions.
pub(crate) const DELEGATE_CONVENTION_NAMES: [&str; 3] = ["getValue", "setValue", "provideDelegate"];

/// Whether a declaration can serve as a delegated-property convention at all.
///
/// A convention is called from a GENERATED accessor, which has no scope to fill an implicit context
/// from, so a context parameter makes the declaration unusable as one. kotlinc says so on the
/// declaration itself and then reports the property as having no applicable `getValue`; this is the
/// one rule both of those answers are derived from, so candidate selection and the declaration
/// check cannot drift apart.
pub(crate) fn is_usable_delegate_convention(is_operator: bool, context_count: usize) -> bool {
    is_operator && context_count == 0
}

/// The `KProperty` classifier every delegated-property convention receives its second argument as.
///
/// ONE definition. It is the classifier applicability is decided against, the type of the value
/// lowering builds, and the type published on the checked plan — three places that must agree, and
/// did not while each spelled the name for itself.
///
/// The identity is the interned [`TypeName`], the same thing every other classifier reference in
/// the compiler carries; the string is the interning key and travels no further. Gating this on
/// `resolver.classifier(...)` answering was tried and reverted: a dependency set without
/// `kotlin.reflect.KProperty` would then fail as an internal checked-FIR error instead of the
/// ordinary "no applicable `getValue`" diagnostic, which is the wrong report and is already the
/// convention selection's job.
pub(crate) fn delegate_property_reference_type() -> Ty {
    Ty::obj_name(crate::types::type_name("kotlin/reflect/KProperty"))
}

/// Select one delegated-property convention from the normalized member/extension family. Operator
/// filtering precedes applicability, so a same-named ordinary function cannot displace the
/// convention declaration.
pub(super) fn select_delegate_operator(
    resolver: &crate::symbol_resolver::SymbolResolver,
    receiver: Ty,
    name: &str,
    args: &[Ty],
) -> crate::symbol_resolver::CandidateSelection<(crate::libraries::FunctionInfo, Ty)> {
    let callables = resolver.receiver_callables(receiver, name);
    crate::trace_compiler!(
        "resolve",
        "delegate operator candidates receiver={receiver:?} name={name} candidates={:?}",
        callables
            .functions()
            .iter()
            .map(|candidate| (
                candidate.kind,
                candidate.flags.operator,
                candidate.flags.inline,
                candidate.semantic_params(),
                candidate.callable.ret,
            ))
            .collect::<Vec<_>>(),
    );
    // A convention operator is called from a GENERATED accessor, which has no scope to fill an
    // implicit context from — so a context parameter makes the declaration unusable as a delegate
    // convention, and kotlinc says so twice: "context parameters on delegation operators are
    // unsupported" on the declaration, then reports the property's delegate as having no applicable
    // `getValue`. Dropping such a candidate here is the second of those: the property gets the
    // ordinary "no convention" diagnostic instead of a selected call whose argument list cannot be
    // mapped onto the declaration's slots.
    let overloads = callables
        .functions()
        .iter()
        .filter(|candidate| {
            is_usable_delegate_convention(candidate.flags.operator, candidate.context_count)
        })
        .cloned()
        .collect::<Vec<_>>();
    let callables =
        crate::libraries::Callables::Functions(crate::libraries::FunctionSet { overloads });
    let args = args
        .iter()
        .copied()
        .map(CallArgKind::Typed)
        .collect::<Vec<_>>();
    match resolver.select_receiver_function_with_params_tracking(
        receiver,
        name,
        &args,
        &[],
        &callables,
        None,
    ) {
        crate::symbol_resolver::CandidateSelection::Selected((selected, _, ret)) => {
            crate::symbol_resolver::CandidateSelection::Selected((selected, ret))
        }
        crate::symbol_resolver::CandidateSelection::None => {
            crate::symbol_resolver::CandidateSelection::None
        }
        crate::symbol_resolver::CandidateSelection::Ambiguous => {
            crate::symbol_resolver::CandidateSelection::Ambiguous
        }
    }
}

impl Checker<'_> {
    /// Classify a convention that was not selected, and render the candidates kotlinc would list.
    ///
    /// The candidate set is every function of that name the delegate's scope offers, whatever
    /// excluded it from selection — kotlinc lists an inapplicable overload, a missing `operator`
    /// modifier and a context-prefixed declaration alike, because each one is a thing the author
    /// plausibly meant to be the convention.
    fn delegate_convention_failure(
        &self,
        delegate_ty: Ty,
        name: &str,
    ) -> DelegateConventionFailure {
        let callables = self.resolver().receiver_callables(delegate_ty, name);
        let candidates = callables
            .functions()
            .iter()
            .map(|candidate| {
                let parameters = candidate
                    .callable
                    .params
                    .iter()
                    .enumerate()
                    .map(|(index, parameter)| {
                        let parameter_name = candidate
                            .call_sig
                            .param_names
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| format!("p{index}"));
                        format!("{parameter_name}: {}", delegate_diagnostic_ty(*parameter))
                    })
                    .collect::<Vec<_>>();
                let (context, value) = parameters.split_at(
                    // A provider states its context count independently of its parameter list; a
                    // disagreement must render a shorter prefix, never index past the list.
                    candidate.context_count.min(parameters.len()),
                );
                let context = if context.is_empty() {
                    String::new()
                } else {
                    format!("context({}) ", context.join(", "))
                };
                format!(
                    "{context}fun {name}({}): {}",
                    value.join(", "),
                    delegate_diagnostic_ty(candidate.callable.ret),
                )
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            DelegateConventionFailure::Missing
        } else {
            DelegateConventionFailure::NoneApplicable(candidates)
        }
    }

    /// Report a convention the delegate does not supply, anchored on `by` as kotlinc anchors it.
    ///
    /// `value` is the written value's type, present exactly for `setValue`: it is the third slot of
    /// the signature the message demands, and the reason the message ends differently.
    fn report_delegate_convention_failure(
        &mut self,
        site: DelegateConventionSite,
        delegate_ty: Ty,
        name: &str,
        this_ref: Ty,
        property: Option<Ty>,
        value: Option<Ty>,
    ) {
        // A delegate whose own type failed to check has already been reported; naming it here would
        // only repeat that failure under a second heading.
        if delegate_ty.mentions_error()
            || delegate_ty.mentions_pending()
            || this_ref.mentions_error()
            || value.is_some_and(|value| value.mentions_error() || value.mentions_pending())
        {
            return;
        }
        let failure = self.delegate_convention_failure(delegate_ty, name);
        // kotlinc names the property reference exactly when one candidate is to blame; with several
        // it falls back to star projections, and with none it never built the reference at all.
        let blamed = match &failure {
            DelegateConventionFailure::NoneApplicable(candidates) => candidates.len() == 1,
            DelegateConventionFailure::Missing => false,
        };
        let reference = property
            .filter(|property| blamed && !property.mentions_error() && !property.mentions_pending())
            .map(|property| site.applied_reference(property))
            .unwrap_or_else(|| site.star_projected_reference());
        let mut signature = vec![delegate_diagnostic_ty(this_ref), reference];
        signature.extend(value.map(delegate_diagnostic_ty));
        let signature = format!("{name}({})", signature.join(", "));
        let message = match failure {
            DelegateConventionFailure::Missing => format!(
                "type '{}' has no method '{signature}', so it cannot serve as a delegate{}.",
                delegate_diagnostic_ty(delegate_ty),
                if value.is_some() {
                    " for var (read-write property)"
                } else {
                    ""
                },
            ),
            DelegateConventionFailure::NoneApplicable(candidates) => format!(
                "property delegate must have a '{signature}' method. None of the following \
                 functions is applicable:\n{}",
                candidates.join("\n"),
            ),
        };
        self.diags.error(site.by_span, message);
    }

    /// Check a delegate expression and select its convention as one contextual operation.
    ///
    /// Generic delegate factories can receive their type argument only from `getValue`'s result.
    /// The expectation-free attempt is postponed: it may establish a constructor/call shape, but
    /// it does not own the final inference decision. Once `getValue` supplies a receiver
    /// expectation, the expression is checked authoritatively against that type.
    pub(super) fn check_delegate_getvalue(
        &mut self,
        scope: &CheckerScope<'_>,
        delegate: ExprId,
        provide_ref: Ty,
        this_ref: Ty,
        expected_property: Option<Ty>,
        site: DelegateConventionSite,
    ) -> (Ty, Option<Ty>) {
        let selection = DelegateGetValueSelection {
            provide_ref,
            this_ref,
            expected_property,
            site,
        };
        let mut receiver_expectation = None;
        loop {
            let delegate_ty = match (receiver_expectation, expected_property) {
                (Some(expected), _) => self.expr_expected(scope, delegate, expected),
                // With no declared property type there is no later result expectation to commit a
                // postponed delegate expression. Its own arguments must therefore finish its type:
                // `val p by ReadOnlyProperty { _, kProperty -> kProperty }` infers the SAM result
                // (and hence `p`) as `KProperty<*>` from the lambda body.
                (None, None) => self.expr(scope, delegate),
                (None, Some(_)) => self.check_postponed_argument(scope, delegate),
            };
            match self.select_delegate_getvalue_attempt(
                scope,
                delegate,
                delegate_ty,
                receiver_expectation,
                selection,
            ) {
                DelegateGetValueAttempt::Complete(result) => {
                    return (delegate_ty, result);
                }
                DelegateGetValueAttempt::RetryWithReceiver(expected) => {
                    receiver_expectation = Some(expected);
                }
            }
        }
    }

    fn select_delegate_getvalue_attempt(
        &mut self,
        scope: &CheckerScope<'_>,
        delegate: ExprId,
        delegate_ty: Ty,
        receiver_expectation: Option<Ty>,
        selection: DelegateGetValueSelection,
    ) -> DelegateGetValueAttempt {
        let DelegateGetValueSelection {
            provide_ref,
            this_ref,
            expected_property,
            site,
        } = selection;
        crate::trace_compiler!(
            "fir",
            "resolve delegate convention delegate={delegate:?} receiver={delegate_ty:?} provide_ref={provide_ref:?} this_ref={this_ref:?}",
        );
        let kproperty = delegate_property_reference_type();
        // Record the classifier the source answered with, so the checked plan and every value built
        // from it carry this exact resolution instead of re-spelling the name downstream.
        self.delegate_property_reference_type = Some(kproperty);
        let provide_target = self.select_delegate_operator(
            scope,
            delegate,
            delegate_ty,
            "provideDelegate",
            &[provide_ref, kproperty],
            None,
        );
        let stored_ty = provide_target
            .as_ref()
            .map(DelegateGetValueTarget::ret)
            .unwrap_or(delegate_ty);
        let Some(target) = self.select_delegate_operator(
            scope,
            delegate,
            stored_ty,
            "getValue",
            &[this_ref, kproperty],
            expected_property,
        ) else {
            self.report_delegate_convention_failure(
                site,
                stored_ty,
                "getValue",
                this_ref,
                expected_property,
                None,
            );
            return DelegateGetValueAttempt::Complete(None);
        };
        if let Some(applied_receiver) = target.applied_receiver().filter(|receiver| {
            if *receiver == delegate_ty
                || receiver.obj_internal() != stored_ty.obj_internal()
                || stored_ty.obj_internal() != delegate_ty.obj_internal()
            {
                return false;
            }
            if delegate_ty.type_args().is_empty() && !receiver.type_args().is_empty() {
                return true;
            }
            let mut bindings = crate::symbol_resolver::GSigBinds::new();
            crate::symbol_resolver::unify_ty(delegate_ty, *receiver, &mut bindings);
            !bindings.is_empty()
                && crate::symbol_resolver::ty_subst_keep_unbound(delegate_ty, &bindings)
                    == *receiver
        }) {
            if applied_receiver != delegate_ty && receiver_expectation != Some(applied_receiver) {
                return DelegateGetValueAttempt::RetryWithReceiver(applied_receiver);
            }
        }
        let ret = target.ret();
        if let Some(expected) = expected_property.filter(|expected| *expected != Ty::Error) {
            let convention_receiver = target.receiver_constrained_by_result(expected);
            let delegate_receiver = match (&provide_target, convention_receiver) {
                (Some(provide), Some(stored)) => provide.receiver_constrained_by_result(stored),
                (None, constrained) => constrained,
                (Some(_), None) => None,
            };
            if let Some(refined) = delegate_receiver
                .and_then(|constraint| {
                    self.resolver()
                        .apply_raw_receiver_constraint(delegate_ty, constraint)
                })
                .map(|receiver| self.refine_delegate_receiver_to_bounds(receiver))
                .filter(|receiver| *receiver != delegate_ty)
            {
                if receiver_expectation != Some(refined) {
                    return DelegateGetValueAttempt::RetryWithReceiver(refined);
                }
            }
            let mut bindings = crate::symbol_resolver::GSigBinds::new();
            crate::symbol_resolver::unify_ty(ret, expected, &mut bindings);
            let refined = crate::symbol_resolver::ty_subst_keep_unbound(delegate_ty, &bindings);
            let refined = match refined {
                Ty::Obj(name, arguments) if arguments.iter().any(|argument| *argument == ret) => {
                    Ty::obj_args_name(
                        name,
                        &arguments
                            .iter()
                            .map(|argument| {
                                if *argument == ret {
                                    expected
                                } else {
                                    *argument
                                }
                            })
                            .collect::<Vec<_>>(),
                    )
                }
                other => other,
            };
            let refined = self.refine_delegate_receiver_to_bounds(refined);
            crate::trace_compiler!(
                "fir",
                "refine delegate from getValue result delegate={delegate:?} ret={ret:?} expected={expected:?} bindings={bindings:?} receiver={delegate_ty:?} refined={refined:?}",
            );
            if refined != delegate_ty {
                if receiver_expectation != Some(refined) {
                    return DelegateGetValueAttempt::RetryWithReceiver(refined);
                }
            }
        }
        crate::trace_compiler!(
            "fir",
            "selected delegate getValue delegate={delegate:?} receiver={stored_ty:?} target={target:?}",
        );
        if let Some(provide_target) = provide_target {
            self.delegate_provide_targets
                .insert(delegate, provide_target);
        }
        self.delegate_getvalue_targets.insert(delegate, target);
        DelegateGetValueAttempt::Complete(Some(ret))
    }

    /// A delegated-result constraint can expose a nullable result around a non-null classifier
    /// variable (`getValue(): T?`, `T : Any`). A provisional unconstrained constructor probe may
    /// have completed that variable as `Any?`; replacing it with the whole expected nullable result
    /// would preserve the same bound violation. Normalize only that nullable shell against the
    /// declaration's actual upper bound, then let the authoritative expected constructor check
    /// validate every source argument again.
    fn refine_delegate_receiver_to_bounds(&self, receiver: Ty) -> Ty {
        let Ty::Obj(owner, arguments) = receiver else {
            return receiver;
        };
        let Some(class) = self.resolved_type_name(owner) else {
            return receiver;
        };
        let mut refined = arguments.to_vec();
        let mut bindings = HashMap::new();
        for (index, formal) in class.type_params().iter().enumerate() {
            let Some(argument) = refined.get(index).copied() else {
                break;
            };
            let bound = class
                .type_param_bounds()
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .filter(|bound| *bound != Ty::Error)
                .map(|bound| crate::symbol_resolver::ty_subst_keep_unbound(bound, &bindings));
            let argument = match bound {
                Some(bound)
                    if argument.is_nullable()
                        && !self.receiver_is_assignable(argument, bound)
                        && self.receiver_is_assignable(argument.non_null(), bound) =>
                {
                    argument.non_null()
                }
                _ => argument,
            };
            refined[index] = argument;
            bindings.insert(formal.clone(), argument);
        }
        Ty::obj_args_name(owner, &refined)
    }

    pub(super) fn record_delegate_setvalue(
        &mut self,
        scope: &CheckerScope<'_>,
        delegate: ExprId,
        _delegate_ty: Ty,
        this_ref: Ty,
        property_ty: Ty,
        site: DelegateConventionSite,
    ) -> Option<()> {
        let delegate_ty = self.expr_types[delegate.0 as usize];
        crate::trace_compiler!(
            "fir",
            "resolve delegate setValue delegate={delegate:?} receiver={delegate_ty:?} this_ref={this_ref:?} property={property_ty:?}",
        );
        let kproperty = delegate_property_reference_type();
        let stored_ty = self
            .delegate_provide_targets
            .get(&delegate)
            .map(DelegateGetValueTarget::ret)
            .unwrap_or(delegate_ty);
        let Some(target) = self.select_delegate_operator(
            scope,
            delegate,
            stored_ty,
            "setValue",
            &[this_ref, kproperty, property_ty],
            None,
        ) else {
            self.report_delegate_convention_failure(
                site,
                stored_ty,
                "setValue",
                this_ref,
                Some(property_ty),
                Some(property_ty),
            );
            return None;
        };
        crate::trace_compiler!("fir", "selected delegate setValue target={target:?}");
        self.delegate_setvalue_targets.insert(delegate, target);
        Some(())
    }

    fn select_delegate_operator(
        &self,
        scope: &CheckerScope<'_>,
        delegate: ExprId,
        delegate_ty: Ty,
        name: &str,
        args: &[Ty],
        expected_result: Option<Ty>,
    ) -> Option<DelegateGetValueTarget> {
        let resolver = self.resolver();
        let callables = resolver.receiver_callables(delegate_ty, name);
        let call_args = args
            .iter()
            .copied()
            .map(CallArgKind::Typed)
            .collect::<Vec<_>>();
        let select_kind = |kind| {
            // Context parameters exclude a candidate here for the same reason as in
            // `select_delegate_operator`: the generated accessor has no scope to fill one from.
            let overloads = callables
                .functions()
                .iter()
                .filter(|candidate| {
                    candidate.kind == kind
                        && is_usable_delegate_convention(
                            candidate.flags.operator,
                            candidate.context_count,
                        )
                })
                .cloned()
                .collect::<Vec<_>>();
            let callables =
                crate::libraries::Callables::Functions(crate::libraries::FunctionSet { overloads });
            match resolver.select_receiver_function_with_applied_receiver_tracking(
                delegate_ty,
                name,
                &call_args,
                &[],
                &callables,
                None,
            ) {
                crate::symbol_resolver::CandidateSelection::Selected((
                    selected,
                    _,
                    ret,
                    applied_receiver,
                )) => Some((selected, ret, applied_receiver)),
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => None,
            }
        };
        let selected = if let Some(selected) = select_kind(crate::libraries::FnKind::Member) {
            selected
        } else {
            let syntax = vec![delegate; args.len()];
            let member_extension = member_extension_function_with(
                &self.fed_source(),
                self,
                &self.implicit_receivers(scope),
                self.file.explicit_context_arguments,
                &|parameters| self.select_context_arguments_with_types(scope, parameters),
                &|_| false,
                &|_, _, _| None,
                &|params, call_sig, slots| {
                    let mapped = call_argument_parameter_indices(
                        slots.args.len(),
                        params.len(),
                        slots.arg_names,
                        slots.trailing_lambda,
                        call_sig,
                    )?;
                    let mut score = 0;
                    for (source, parameter) in mapped.into_iter().enumerate() {
                        let expected = *params.get(parameter)?;
                        let actual = *args.get(source)?;
                        if !call_sig.parameter_admits(parameter, expected, actual) {
                            return None;
                        }
                        score += self.member_argument_score(expected, actual)?;
                    }
                    Some(CallCandidateScore {
                        rank: (score, std::cmp::Reverse(0), !call_sig.vararg),
                        sam_signatures: vec![None; slots.args.len()],
                    })
                },
                MemberExtensionFunctionCall {
                    extension_receiver: delegate_ty,
                    result_constraint: CallResultConstraint::direct(expected_result),
                    name,
                    args: &syntax,
                    arg_tys: args,
                    arg_names: None,
                    explicit_type_args: &[],
                    trailing_lambda: false,
                },
                MemberExtensionSelection::Operators,
            )
            .ok()
            .flatten();
            if let Some(selected) = member_extension {
                if !self.member_accessible(selected.visibility, selected.owner) {
                    return None;
                }
                let interface = resolver
                    .classifier(selected.owner)
                    .is_some_and(|shape| shape.is_interface());
                // The DECLARED value parameters, un-erased: the current-module declaration's own
                // signature where there is one, otherwise the callable's generic signature. Never
                // `physical_params`, which is already a target's erasure of this.
                let declared_params = selected
                    .stable_declaration
                    .and_then(|declaration| self.stable_member_declared_shape(declaration))
                    .map(|(_, parameters, _)| parameters)
                    .unwrap_or_else(|| selected.declared_params.clone());
                return Some(DelegateGetValueTarget::MemberExtension {
                    stable_declaration: selected.stable_declaration,
                    external_identity: selected.external_identity,
                    external_default_provider: selected.external_default_provider,
                    owner: selected.owner,
                    name: selected.physical_name,
                    extension_receiver: selected.extension_receiver,
                    dispatch_receiver: self.implicit_receiver_selection(selected.dispatch_receiver),
                    context_count: selected.context_args.len(),
                    params: selected.params,
                    ret: selected.ret,
                    physical_params: selected.physical_params,
                    physical_ret: selected.physical_ret,
                    inline: selected.inline,
                    inline_body_plan: selected.inline_body_plan,
                    suspend: selected.suspend,
                    declared_params,
                    declared_ret: selected.declared_ret,
                    interface,
                });
            }
            select_kind(crate::libraries::FnKind::Extension)?
        };
        let (selected, ret, applied_receiver) = selected;
        let (declared_receiver, declared_params, declared_ret) = selected
            .stable_declaration
            .and_then(|declaration| self.stable_member_declared_shape(declaration))
            .unwrap_or_else(|| {
                (
                    selected.semantic_receiver().unwrap_or(delegate_ty),
                    selected.semantic_signature().params.clone(),
                    selected.semantic_signature().ret,
                )
            });
        if selected.is_extension() {
            let stable_declaration = selected.stable_declaration;
            return resolver
                .build_extension_callable(name, delegate_ty, args, &[], &selected)
                .map(Box::new)
                .map(|callable| DelegateGetValueTarget::Extension {
                    callable,
                    stable_declaration,
                });
        }
        let internal = delegate_ty.obj_internal()?;
        let resolved = resolver.materialize_member_function(delegate_ty, &call_args, &[], selected);
        let owner = resolved.member.owner.unwrap_or(internal);
        let interface = resolved.member.is_interface()
            || resolver
                .classifier(owner)
                .is_some_and(|classifier| classifier.is_interface());
        Some(DelegateGetValueTarget::Member {
            applied_receiver,
            declared_receiver,
            declared_params,
            declared_ret,
            stable_declaration: resolved.member.stable_declaration,
            external_identity: resolved.member.external_identity,
            external_default_provider: resolved.member.external_default_provider,
            owner,
            name: resolved
                .member
                .physical_name
                .clone()
                .unwrap_or_else(|| resolved.member.name.clone()),
            params: resolved.member.params,
            ret,
            physical_params: resolved.physical_params,
            physical_ret: resolved.member.physical_ret,
            descriptor: resolved.member.descriptor,
            interface,
        })
    }

    /// The DECLARED shape of a current-module convention: its receiver, its value parameters and
    /// its result, all as the declaration spells them — a type parameter stays a type parameter.
    ///
    /// This is deliberately not `physical_params`/`physical_ret`, which are the ABI slots after
    /// erasure. A consumer that needs to know a value crosses into a `T` slot must be told `T`;
    /// what `T` costs physically is the target's answer, and a different target gives a different
    /// one.
    fn stable_member_declared_shape(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<(Ty, Vec<Ty>, Ty)> {
        let index = self.resolved_index?;
        let owner = index.declaration_anchor(declaration)?.owner?;
        let classifier = index.classifier_header(owner)?.classifier;
        let arguments = index
            .classifier_type_arguments(owner)?
            .iter()
            .map(|parameter| {
                let header = index.type_parameter_header(*parameter)?;
                let name = index.type_parameter_semantic_name(*parameter)?;
                let bound = header
                    .bounds
                    .first()
                    .map(|bound| bound.ty.get())
                    .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
                Some(Ty::ty_param(name, bound))
            })
            .collect::<Option<Vec<_>>>()?;
        let receiver = Ty::obj_args_name(classifier, &arguments);
        let signature = index.signature(declaration)?;
        let parameters = signature
            .parameters
            .iter()
            .map(|parameter| parameter.get())
            .collect::<Vec<_>>();
        let result = signature.result.get();
        Some((receiver, parameters, result))
    }
}
