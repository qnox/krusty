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

enum DelegateOperatorSelection {
    /// Boxed: `DelegateGetValueTarget` is the only large payload here, and this enum is returned
    /// through several convention rungs.
    Selected(Box<DelegateGetValueTarget>),
    None,
    Failure(DelegateOperatorFailure),
}

enum OrdinaryDelegateSelection {
    None(Vec<crate::libraries::FunctionInfo>),
    /// `FunctionInfo` alone is ~1.2 KiB and is what makes this variant large; the two `Ty`s are
    /// `Copy` and a couple of words each, so only the callable is boxed.
    Selected(Box<crate::libraries::FunctionInfo>, Ty, Ty),
    Ambiguous(Vec<crate::libraries::FunctionInfo>),
}

enum DelegateOperatorFailure {
    AmbiguousOrdinary(Vec<crate::libraries::FunctionInfo>),
    InapplicableCandidates {
        // Keep the convention tower's semantic rungs separate. Selection may continue to a lower
        // rung to find an applicable declaration, but on total failure Kotlin diagnoses only the
        // earliest rung that contributed same-name candidates.
        members: Vec<crate::libraries::FunctionInfo>,
        member_extensions:
            Vec<super::member_extension_selection::MemberExtensionConventionDiagnosticCandidate>,
        extensions: Vec<crate::libraries::FunctionInfo>,
    },
    AmbiguousMemberExtensions(Vec<MemberExtensionFunctionCandidate>),
}

#[derive(Clone)]
struct DelegateGetValueSelection {
    provide_ref: Ty,
    this_ref: Ty,
    expected_property: Option<Ty>,
    site: DelegateConventionSite,
}

/// What kotlinc's delegate-convention diagnostics name beyond the types already in hand: where the
/// `by` keyword sits, which receivers the property has, and whether it is a `var`. The receivers are
/// listed dispatch first — the order `KProperty2<D, E, V>` spells them.
#[derive(Clone)]
pub(crate) struct DelegateConventionSite {
    pub(crate) by_span: Span,
    pub(crate) dispatch_receiver: Option<Ty>,
    pub(crate) extension_receiver: Option<Ty>,
    pub(crate) dispatch_diagnostic_name: Option<DelegateDispatchDiagnosticName>,
    pub(crate) is_var: bool,
}

#[derive(Clone)]
pub(crate) enum DelegateDispatchDiagnosticName {
    Source(Box<str>),
    Anonymous,
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
            dispatch_diagnostic_name: None,
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
            dispatch_diagnostic_name: None,
            is_var,
        }
    }

    fn receivers(&self) -> Vec<Ty> {
        self.dispatch_receiver
            .into_iter()
            .chain(self.extension_receiver)
            .collect()
    }

    fn flavour(&self) -> String {
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
    fn star_projected_reference(&self) -> String {
        let arguments = vec!["*"; self.receivers().len() + 1].join(", ");
        format!("{}<{arguments}>", self.flavour())
    }

    /// `KMutableProperty1<Holder, Long>` — the reference the accessors would really pass: this
    /// property's receivers, then its type.
    fn applied_reference(&self, property: Ty) -> String {
        let mut arguments = Vec::new();
        if let Some(dispatch) = self.dispatch_receiver {
            arguments.push(self.property_reference_dispatch_ty(dispatch));
        }
        if let Some(extension) = self.extension_receiver {
            arguments.push(delegate_diagnostic_ty(extension));
        }
        arguments.push(delegate_diagnostic_ty(property));
        let arguments = arguments.join(", ");
        format!("{}<{arguments}>", self.flavour())
    }

    /// The anonymous dispatch receiver has a source spelling for the convention's `thisRef`, but
    /// no denotable classifier type for the `KProperty1` argument. Kotlin therefore renders that
    /// type argument as a star projection while still naming the first argument `<anonymous>`.
    fn property_reference_dispatch_ty(&self, ty: Ty) -> String {
        debug_assert_eq!(self.dispatch_receiver, Some(ty));
        match &self.dispatch_diagnostic_name {
            Some(DelegateDispatchDiagnosticName::Source(name)) => name.to_string(),
            Some(DelegateDispatchDiagnosticName::Anonymous) => "*".to_string(),
            None => panic!("a delegate dispatch diagnostic must retain its source kind"),
        }
    }

    fn this_ref_diagnostic_ty(&self, ty: Ty) -> String {
        if let Some(extension) = self.extension_receiver {
            debug_assert_eq!(extension, ty);
            return delegate_diagnostic_ty(extension);
        }
        if let Some(dispatch) = self.dispatch_receiver {
            debug_assert_eq!(dispatch, ty);
            match &self.dispatch_diagnostic_name {
                Some(DelegateDispatchDiagnosticName::Source(name)) => return name.to_string(),
                Some(DelegateDispatchDiagnosticName::Anonymous) => {
                    return "<anonymous>".to_string()
                }
                None => panic!("a delegate dispatch diagnostic must retain its source kind"),
            }
        }
        delegate_diagnostic_ty(ty)
    }
}

/// Why no convention was selected, in the three shapes kotlinc reports.
enum DelegateConventionFailure {
    /// Nothing of that name is visible on the delegate at all.
    Missing,
    /// Functions of that name exist and none is applicable. Carries them rendered as kotlinc lists
    /// them, in the order the delegate's scope offers them, and — when exactly one is to blame —
    /// the result type kotlinc then reports the property as having.
    NoneApplicable(Vec<String>, Option<Ty>),
    /// More than one convention is applicable and none is more specific.
    Ambiguous(Vec<String>),
}

/// One convention candidate in the shared diagnostic vocabulary. Member-extension lookup reaches
/// declarations through an implicit dispatch receiver, so those candidates are not present in
/// `receiver_callables(delegate, name)` and must cross that lookup boundary explicitly.
#[derive(Clone)]
pub(crate) struct DelegateConventionDiagnosticCandidate {
    rendered: String,
    result: Ty,
}

pub(crate) enum DelegateConventionSelection {
    None(Vec<crate::libraries::FunctionInfo>),
    /// See [`OrdinaryDelegateSelection::Selected`]: the callable is the large half.
    Selected(Box<crate::libraries::FunctionInfo>, Ty),
    Ambiguous(Vec<crate::libraries::FunctionInfo>),
}

/// Render a type the way a delegate diagnostic names it. The literal null type prints as `Nothing?`,
/// which is what kotlinc calls the `thisRef` a receiverless property passes.
fn delegate_diagnostic_ty(ty: Ty) -> String {
    if ty == Ty::Null {
        return "Nothing?".to_string();
    }
    ty.source_name()
}

pub(crate) fn delegate_convention_diagnostic_candidate(
    name: &str,
    extension_receiver: Option<Ty>,
    context_parameters: &[Ty],
    value_parameters: &[Ty],
    parameter_names: &[String],
    result: Ty,
) -> DelegateConventionDiagnosticCandidate {
    let render_parameters = |parameters: &[Ty], offset: usize| {
        parameters
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                let ordinal = offset + index;
                let parameter_name = match parameter_names.get(ordinal) {
                    Some(name) => name.clone(),
                    None => format!("p{ordinal}"),
                };
                format!("{parameter_name}: {}", delegate_diagnostic_ty(*parameter))
            })
            .collect::<Vec<_>>()
    };
    let context = render_parameters(context_parameters, 0);
    let value = render_parameters(value_parameters, context_parameters.len());
    let context = if context.is_empty() {
        String::new()
    } else {
        format!("context({}) ", context.join(", "))
    };
    let callable_name = match extension_receiver {
        Some(receiver) => format!("{}.{}", delegate_diagnostic_ty(receiver), name),
        None => name.to_string(),
    };
    DelegateConventionDiagnosticCandidate {
        rendered: format!(
            "{context}fun {callable_name}({}): {}",
            value.join(", "),
            delegate_diagnostic_ty(result),
        ),
        result,
    }
}

pub(super) fn select_delegate_operator_return(
    resolver: &crate::symbol_resolver::SymbolResolver,
    receiver: Ty,
    name: &str,
    args: &[Ty],
) -> Option<Ty> {
    match select_delegate_operator(resolver, receiver, name, args) {
        DelegateConventionSelection::Selected(_, ret) => Some(ret),
        DelegateConventionSelection::None(_) | DelegateConventionSelection::Ambiguous(_) => None,
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

/// The `KProperty` classifier every delegated-property convention receives its second argument as,
/// RESOLVED through the symbol source that answers applicability.
///
/// ONE definition. It is the classifier applicability is decided against, the type of the value
/// lowering builds, and the type published on the checked plan — three places that must agree, and
/// did not while each spelled the name for itself. Interning the spelling gives a stable
/// [`TypeName`]; it does not mean the configured dependency set declares the classifier, so the
/// declaration is asked for here rather than assumed.
///
/// `None` when it does not. That is deliberately NOT an internal error: without
/// `kotlin.reflect.KProperty` no `getValue`/`setValue` declaration can apply to the delegate, so
/// the property takes the ordinary "no applicable convention" diagnostic — the same report a
/// missing operator gives — instead of a checked-FIR failure naming a classifier the source never
/// wrote.
pub(crate) fn delegate_property_reference_type(
    resolver: &crate::symbol_resolver::SymbolResolver,
) -> Option<Ty> {
    let classifier = crate::types::type_name("kotlin/reflect/KProperty");
    resolver
        .classifier(classifier)
        .map(|_| Ty::obj_name(classifier))
}

/// Select one delegated-property convention from the normalized member/extension family. Operator
/// filtering precedes applicability, so a same-named ordinary function cannot displace the
/// convention declaration.
pub(super) fn select_delegate_operator(
    resolver: &crate::symbol_resolver::SymbolResolver,
    receiver: Ty,
    name: &str,
    args: &[Ty],
) -> DelegateConventionSelection {
    select_delegate_operator_in_kind(resolver, receiver, name, args, None)
}

pub(super) fn select_delegate_operator_kind(
    resolver: &crate::symbol_resolver::SymbolResolver,
    receiver: Ty,
    name: &str,
    args: &[Ty],
    kind: crate::libraries::FnKind,
) -> DelegateConventionSelection {
    select_delegate_operator_in_kind(resolver, receiver, name, args, Some(kind))
}

fn select_delegate_operator_in_kind(
    resolver: &crate::symbol_resolver::SymbolResolver,
    receiver: Ty,
    name: &str,
    args: &[Ty],
    kind: Option<crate::libraries::FnKind>,
) -> DelegateConventionSelection {
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
    let diagnostic_candidates = callables
        .functions()
        .iter()
        .filter(|candidate| kind.is_none_or(|kind| candidate.kind == kind))
        .cloned()
        .collect::<Vec<_>>();
    let overloads = diagnostic_candidates
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
    match resolver.select_receiver_function_with_applied_receiver_tracking(
        receiver,
        name,
        &args,
        &[],
        &callables,
        None,
    ) {
        crate::symbol_resolver::ReceiverFunctionSelection::Selected((selected, _, ret, _)) => {
            DelegateConventionSelection::Selected(selected, ret)
        }
        crate::symbol_resolver::ReceiverFunctionSelection::None => {
            DelegateConventionSelection::None(diagnostic_candidates)
        }
        crate::symbol_resolver::ReceiverFunctionSelection::Ambiguous(candidates) => {
            DelegateConventionSelection::Ambiguous(candidates)
        }
    }
}

/// Classify a convention that was not selected, and render the candidates kotlinc would list.
///
/// The candidate set is every function of that name the delegate's scope offers, whatever
/// excluded it from selection — kotlinc lists an inapplicable overload, a missing `operator`
/// modifier and a context-prefixed declaration alike, because each one is a thing the author
/// plausibly meant to be the convention.
fn delegate_convention_candidates(
    resolver: &crate::symbol_resolver::SymbolResolver,
    delegate_ty: Ty,
    name: &str,
    candidate_result: &mut dyn FnMut(
        &crate::libraries::FunctionInfo,
    ) -> Result<Ty, crate::fir::DiagnosticId>,
) -> Result<Vec<DelegateConventionDiagnosticCandidate>, crate::fir::DiagnosticId> {
    let callables = resolver.receiver_callables(delegate_ty, name);
    delegate_convention_candidates_from_functions(callables.functions(), name, candidate_result)
}

pub(super) fn delegate_convention_candidates_from_functions(
    functions: &[crate::libraries::FunctionInfo],
    name: &str,
    candidate_result: &mut dyn FnMut(
        &crate::libraries::FunctionInfo,
    ) -> Result<Ty, crate::fir::DiagnosticId>,
) -> Result<Vec<DelegateConventionDiagnosticCandidate>, crate::fir::DiagnosticId> {
    functions
        .iter()
        .map(|candidate| {
            let parameters = candidate.semantic_params();
            let (context, value) = parameters.split_at(
                // A provider states its context count independently of its parameter list; a
                // disagreement must render a shorter prefix, never index past the list.
                candidate.context_count.min(parameters.len()),
            );
            let result = candidate_result(candidate)?;
            Ok(delegate_convention_diagnostic_candidate(
                name,
                candidate.is_extension().then(|| {
                    candidate
                        .semantic_receiver()
                        .expect("an extension convention candidate retains its receiver")
                }),
                context,
                value,
                &candidate.call_sig.param_names,
                result,
            ))
        })
        .collect::<Result<Vec<_>, crate::fir::DiagnosticId>>()
}

fn delegate_convention_failure(
    resolver: &crate::symbol_resolver::SymbolResolver,
    delegate_ty: Ty,
    name: &str,
    candidate_result: &mut dyn FnMut(
        &crate::libraries::FunctionInfo,
    ) -> Result<Ty, crate::fir::DiagnosticId>,
) -> Result<DelegateConventionFailure, crate::fir::DiagnosticId> {
    let candidates = delegate_convention_candidates(resolver, delegate_ty, name, candidate_result)?;
    Ok(if candidates.is_empty() {
        DelegateConventionFailure::Missing
    } else {
        let sole_result = match candidates.as_slice() {
            [candidate] => Some(candidate.result),
            _ => None,
        };
        DelegateConventionFailure::NoneApplicable(
            candidates
                .into_iter()
                .map(|candidate| candidate.rendered)
                .collect(),
            sole_result,
        )
    })
}

fn delegate_convention_candidates_failure(
    candidates: &[DelegateConventionDiagnosticCandidate],
) -> DelegateConventionFailure {
    let sole_result = match candidates {
        [candidate] => Some(candidate.result),
        _ => None,
    };
    DelegateConventionFailure::NoneApplicable(
        candidates
            .iter()
            .map(|candidate| candidate.rendered.clone())
            .collect(),
        sole_result,
    )
}

fn delegate_convention_candidates_ambiguity(
    candidates: &[DelegateConventionDiagnosticCandidate],
) -> DelegateConventionFailure {
    DelegateConventionFailure::Ambiguous(
        candidates
            .iter()
            .map(|candidate| candidate.rendered.clone())
            .collect(),
    )
}

fn render_delegate_convention_failure(
    site: DelegateConventionSite,
    delegate_ty: Ty,
    name: &str,
    this_ref: Ty,
    property: Option<Ty>,
    value: Option<Ty>,
    failure: DelegateConventionFailure,
) -> Option<String> {
    if delegate_ty.mentions_error()
        || delegate_ty.mentions_pending()
        || this_ref.mentions_error()
        || value.is_some_and(|value| value.mentions_error() || value.mentions_pending())
    {
        return None;
    }
    // kotlinc names the property reference exactly when one candidate is to blame; with several
    // it falls back to star projections, and with none it never built the reference at all.
    let blamed = match &failure {
        DelegateConventionFailure::NoneApplicable(candidates, _) => candidates.len() == 1,
        DelegateConventionFailure::Missing | DelegateConventionFailure::Ambiguous(_) => false,
    };
    // With no declared type of its own, the property's type is the one candidate's result — which
    // is what kotlinc then names in the reference it demands.
    let sole_result = match &failure {
        DelegateConventionFailure::NoneApplicable(_, result) => *result,
        DelegateConventionFailure::Missing | DelegateConventionFailure::Ambiguous(_) => None,
    };
    let applied_reference = property
        .filter(|property| !property.mentions_error() && !property.mentions_pending())
        .or(sole_result)
        .filter(|property| blamed && !property.mentions_error() && !property.mentions_pending())
        .map(|property| site.applied_reference(property));
    let reference = match applied_reference {
        Some(reference) => reference,
        None => site.star_projected_reference(),
    };
    let mut signature = vec![site.this_ref_diagnostic_ty(this_ref), reference];
    signature.extend(value.map(delegate_diagnostic_ty));
    let signature = format!("{name}({})", signature.join(", "));
    Some(match failure {
        DelegateConventionFailure::Missing => format!(
            "type '{}' has no method '{signature}', so it cannot serve as a delegate{}.",
            delegate_diagnostic_ty(delegate_ty),
            if value.is_some() {
                " for var (read-write property)"
            } else {
                ""
            },
        ),
        DelegateConventionFailure::NoneApplicable(candidates, _) => format!(
            "property delegate must have a '{signature}' method. None of the following \
             functions is applicable:\n{}",
            candidates.join("\n"),
        ),
        DelegateConventionFailure::Ambiguous(candidates) => format!(
            "overload resolution ambiguity on method '{signature}':\n{}",
            candidates.join("\n"),
        ),
    })
}

pub(crate) fn delegate_convention_message_with_candidates(
    site: DelegateConventionSite,
    delegate_ty: Ty,
    name: &str,
    this_ref: Ty,
    property: Option<Ty>,
    value: Option<Ty>,
    candidates: &[DelegateConventionDiagnosticCandidate],
) -> Option<String> {
    render_delegate_convention_failure(
        site,
        delegate_ty,
        name,
        this_ref,
        property,
        value,
        delegate_convention_candidates_failure(candidates),
    )
}

pub(crate) fn delegate_convention_ambiguity_message_with_candidates(
    site: DelegateConventionSite,
    delegate_ty: Ty,
    name: &str,
    this_ref: Ty,
    property: Option<Ty>,
    value: Option<Ty>,
    candidates: &[DelegateConventionDiagnosticCandidate],
) -> Option<String> {
    render_delegate_convention_failure(
        site,
        delegate_ty,
        name,
        this_ref,
        property,
        value,
        delegate_convention_candidates_ambiguity(candidates),
    )
}

pub(crate) fn delegate_convention_ambiguity_message_with_functions(
    site: DelegateConventionSite,
    delegate_ty: Ty,
    name: &str,
    this_ref: Ty,
    property: Option<Ty>,
    value: Option<Ty>,
    candidates: &[crate::libraries::FunctionInfo],
    candidate_result: &mut dyn FnMut(
        &crate::libraries::FunctionInfo,
    ) -> Result<Ty, crate::fir::DiagnosticId>,
) -> Result<Option<String>, crate::fir::DiagnosticId> {
    if delegate_ty.mentions_error()
        || delegate_ty.mentions_pending()
        || this_ref.mentions_error()
        || value.is_some_and(|value| value.mentions_error() || value.mentions_pending())
    {
        return Ok(None);
    }
    let candidates =
        delegate_convention_candidates_from_functions(candidates, name, candidate_result)?;
    Ok(delegate_convention_ambiguity_message_with_candidates(
        site,
        delegate_ty,
        name,
        this_ref,
        property,
        value,
        &candidates,
    ))
}

/// The exact wording kotlinc gives a convention the delegate does not supply, or `None` when the
/// failure must stay silent.
///
/// `value` is the written value's type, present exactly for `setValue`: it is the third slot of the
/// signature the message demands, and the reason the message ends differently.
///
/// This is a free function because the SIGNATURE phase needs the identical wording: a delegated
/// property with no declared type cannot finalize once `getValue` is missing, so the body check
/// that would otherwise report it never runs for that declaration. Both callers producing the same
/// string is what lets the duplicate collapse if they ever both reach the sink.
pub(crate) fn delegate_convention_message(
    resolver: &crate::symbol_resolver::SymbolResolver,
    site: DelegateConventionSite,
    delegate_ty: Ty,
    name: &str,
    this_ref: Ty,
    property: Option<Ty>,
    value: Option<Ty>,
    candidate_result: &mut dyn FnMut(
        &crate::libraries::FunctionInfo,
    ) -> Result<Ty, crate::fir::DiagnosticId>,
) -> Result<Option<String>, crate::fir::DiagnosticId> {
    // Do not demand or render candidates when the convention's own operand facts already failed.
    // Their source diagnostics are authoritative and this convention message would only cascade.
    if delegate_ty.mentions_error()
        || delegate_ty.mentions_pending()
        || this_ref.mentions_error()
        || value.is_some_and(|value| value.mentions_error() || value.mentions_pending())
    {
        return Ok(None);
    }
    let failure = delegate_convention_failure(resolver, delegate_ty, name, candidate_result)?;
    Ok(render_delegate_convention_failure(
        site,
        delegate_ty,
        name,
        this_ref,
        property,
        value,
        failure,
    ))
}

impl Checker<'_> {
    /// Report a convention the delegate does not supply, anchored on `by` as kotlinc anchors it.
    fn report_delegate_convention_failure(
        &mut self,
        site: DelegateConventionSite,
        delegate_ty: Ty,
        name: &str,
        this_ref: Ty,
        property: Option<Ty>,
        value: Option<Ty>,
    ) {
        // The checker renders a candidate from the type its own declaration already carries; the
        // signature phase, where one may still be undetermined, resolves it first.
        let message = match delegate_convention_message(
            &self.resolver(),
            site.clone(),
            delegate_ty,
            name,
            this_ref,
            property,
            value,
            &mut |candidate| Ok(candidate.callable.ret),
        ) {
            Ok(message) => message,
            Err(_) => {
                panic!("a checked delegate convention candidate must have a finalized result")
            }
        };
        let Some(message) = message else {
            return;
        };
        self.diags.error(site.by_span, message);
    }

    fn report_delegate_convention_ambiguity(
        &mut self,
        site: DelegateConventionSite,
        delegate_ty: Ty,
        name: &str,
        this_ref: Ty,
        property: Option<Ty>,
        value: Option<Ty>,
        candidates: &[crate::libraries::FunctionInfo],
    ) {
        let message = match delegate_convention_ambiguity_message_with_functions(
            site.clone(),
            delegate_ty,
            name,
            this_ref,
            property,
            value,
            candidates,
            &mut |candidate| Ok(candidate.callable.ret),
        ) {
            Ok(message) => message,
            Err(_) => panic!("a checked ambiguous convention candidate has a finalized result"),
        };
        if let Some(message) = message {
            self.diags.error(site.by_span, message);
        }
    }

    fn report_member_extension_delegate_convention_ambiguity(
        &mut self,
        site: DelegateConventionSite,
        delegate_ty: Ty,
        name: &str,
        this_ref: Ty,
        property: Option<Ty>,
        value: Option<Ty>,
        candidates: &[MemberExtensionFunctionCandidate],
    ) {
        let candidates = candidates
            .iter()
            .map(|candidate| {
                let result = if candidate.ret.mentions_pending() {
                    let declaration = candidate.stable_declaration.expect(
                        "an undetermined member-extension delegate candidate retains its declaration",
                    );
                    self.resolved_index
                        .and_then(|index| index.signature(declaration))
                        .map(|signature| signature.result.get())
                        .expect(
                            "a checked member-extension delegate candidate has a finalized result",
                        )
                } else {
                    candidate.ret
                };
                let context_count = candidate.context_count.min(candidate.params.len());
                let (context, parameters) = candidate.params.split_at(context_count);
                delegate_convention_diagnostic_candidate(
                    name,
                    Some(candidate.extension_receiver),
                    context,
                    parameters,
                    &candidate.diagnostic_param_names,
                    result,
                )
            })
            .collect::<Vec<_>>();
        let message = delegate_convention_ambiguity_message_with_candidates(
            site.clone(),
            delegate_ty,
            name,
            this_ref,
            property,
            value,
            &candidates,
        )
        .expect("ambiguous member-extension delegate candidates must produce a diagnostic");
        self.diags.error(site.by_span, message);
    }

    #[allow(clippy::too_many_arguments)]
    fn report_inapplicable_delegate_convention_failure(
        &mut self,
        site: DelegateConventionSite,
        delegate_ty: Ty,
        name: &str,
        this_ref: Ty,
        property: Option<Ty>,
        value: Option<Ty>,
        members: &[crate::libraries::FunctionInfo],
        member_extensions: &[super::member_extension_selection::MemberExtensionConventionDiagnosticCandidate],
        extensions: &[crate::libraries::FunctionInfo],
    ) {
        let ordinary = |functions: &[crate::libraries::FunctionInfo]| {
            delegate_convention_candidates_from_functions(functions, name, &mut |candidate| {
                Ok(candidate.callable.ret)
            })
            .expect("a checked delegate convention candidate has a finalized result")
        };
        let member_extension_candidates =
            |checker: &Self| -> Vec<DelegateConventionDiagnosticCandidate> {
                member_extensions
                    .iter()
                    .map(|candidate| {
                        let result = if candidate.ret.mentions_pending() {
                            let declaration = candidate.stable_declaration.expect(
                        "an undetermined excluded delegate candidate retains its declaration",
                    );
                            checker
                                .resolved_index
                                .and_then(|index| index.signature(declaration))
                                .map(|signature| signature.result.get())
                                .expect("an excluded delegate candidate has a finalized result")
                        } else {
                            candidate.ret
                        };
                        let context_count = candidate.context_count.min(candidate.params.len());
                        let (context, parameters) = candidate.params.split_at(context_count);
                        delegate_convention_diagnostic_candidate(
                            name,
                            Some(candidate.extension_receiver),
                            context,
                            parameters,
                            &candidate.parameter_names,
                            result,
                        )
                    })
                    .collect()
            };
        // Kotlin 2.4.20 lists every rung's candidates, members first; earlier releases diagnosed
        // only the earliest rung that contributed any.
        let candidates =
            if crate::kotlin_version::at_least(crate::kotlin_version::KotlinVersion::V2_4_20) {
                let mut all = ordinary(members);
                all.extend(member_extension_candidates(self));
                all.extend(ordinary(extensions));
                all
            } else if !members.is_empty() {
                ordinary(members)
            } else if !member_extensions.is_empty() {
                member_extension_candidates(self)
            } else {
                ordinary(extensions)
            };
        let message = delegate_convention_message_with_candidates(
            site.clone(),
            delegate_ty,
            name,
            this_ref,
            property,
            value,
            &candidates,
        )
        .expect("retained delegate candidates must produce a diagnostic");
        self.diags.error(site.by_span, message);
    }

    #[allow(clippy::too_many_arguments)]
    fn report_delegate_operator_failure(
        &mut self,
        site: DelegateConventionSite,
        delegate_ty: Ty,
        name: &str,
        this_ref: Ty,
        property: Option<Ty>,
        value: Option<Ty>,
        failure: DelegateOperatorFailure,
    ) {
        match failure {
            DelegateOperatorFailure::AmbiguousOrdinary(candidates) => self
                .report_delegate_convention_ambiguity(
                    site,
                    delegate_ty,
                    name,
                    this_ref,
                    property,
                    value,
                    &candidates,
                ),
            DelegateOperatorFailure::InapplicableCandidates {
                members,
                member_extensions,
                extensions,
            } => self.report_inapplicable_delegate_convention_failure(
                site,
                delegate_ty,
                name,
                this_ref,
                property,
                value,
                &members,
                &member_extensions,
                &extensions,
            ),
            DelegateOperatorFailure::AmbiguousMemberExtensions(candidates) => self
                .report_member_extension_delegate_convention_ambiguity(
                    site,
                    delegate_ty,
                    name,
                    this_ref,
                    property,
                    value,
                    &candidates,
                ),
        }
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
                selection.clone(),
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
        // Record the classifier the source answered with, so the checked plan and every value built
        // from it carry this exact resolution instead of re-spelling the name downstream. When the
        // configured dependency set does not declare it, no convention can apply, and the property
        // takes the ordinary "no applicable `getValue`" report rather than an internal failure.
        let Some(kproperty) = delegate_property_reference_type(&self.resolver()) else {
            self.report_delegate_convention_failure(
                site,
                delegate_ty,
                "getValue",
                this_ref,
                expected_property,
                None,
            );
            return DelegateGetValueAttempt::Complete(None);
        };
        self.delegate_property_reference_type = Some(kproperty);
        let provide_target = match self.select_delegate_operator(
            scope,
            delegate,
            delegate_ty,
            "provideDelegate",
            &[provide_ref, kproperty],
            None,
        ) {
            DelegateOperatorSelection::Selected(target) => Some(target),
            DelegateOperatorSelection::None => None,
            DelegateOperatorSelection::Failure(failure) => {
                self.report_delegate_operator_failure(
                    site,
                    delegate_ty,
                    "provideDelegate",
                    provide_ref,
                    expected_property,
                    None,
                    failure,
                );
                return DelegateGetValueAttempt::Complete(None);
            }
        };
        let stored_ty = match &provide_target {
            Some(target) => target.ret(),
            None => delegate_ty,
        };
        let target = match self.select_delegate_operator(
            scope,
            delegate,
            stored_ty,
            "getValue",
            &[this_ref, kproperty],
            expected_property,
        ) {
            DelegateOperatorSelection::Selected(target) => target,
            DelegateOperatorSelection::None => {
                self.report_delegate_convention_failure(
                    site,
                    stored_ty,
                    "getValue",
                    this_ref,
                    expected_property,
                    None,
                );
                return DelegateGetValueAttempt::Complete(None);
            }
            DelegateOperatorSelection::Failure(failure) => {
                self.report_delegate_operator_failure(
                    site,
                    stored_ty,
                    "getValue",
                    this_ref,
                    expected_property,
                    None,
                    failure,
                );
                return DelegateGetValueAttempt::Complete(None);
            }
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
                .insert(delegate, *provide_target);
        }
        self.delegate_getvalue_targets.insert(delegate, *target);
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
        let Some(kproperty) = delegate_property_reference_type(&self.resolver()) else {
            self.report_delegate_convention_failure(
                site,
                delegate_ty,
                "setValue",
                this_ref,
                Some(property_ty),
                Some(property_ty),
            );
            return None;
        };
        let stored_ty = match self.delegate_provide_targets.get(&delegate) {
            Some(target) => target.ret(),
            None => delegate_ty,
        };
        let target = match self.select_delegate_operator(
            scope,
            delegate,
            stored_ty,
            "setValue",
            &[this_ref, kproperty, property_ty],
            None,
        ) {
            DelegateOperatorSelection::Selected(target) => target,
            DelegateOperatorSelection::None => {
                self.report_delegate_convention_failure(
                    site,
                    stored_ty,
                    "setValue",
                    this_ref,
                    Some(property_ty),
                    Some(property_ty),
                );
                return None;
            }
            DelegateOperatorSelection::Failure(failure) => {
                self.report_delegate_operator_failure(
                    site,
                    stored_ty,
                    "setValue",
                    this_ref,
                    Some(property_ty),
                    Some(property_ty),
                    failure,
                );
                return None;
            }
        };
        crate::trace_compiler!("fir", "selected delegate setValue target={target:?}");
        self.delegate_setvalue_targets.insert(delegate, *target);
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
    ) -> DelegateOperatorSelection {
        let resolver = self.resolver();
        let optional = name == DELEGATE_CONVENTION_NAMES[2];
        let callables = resolver.receiver_callables(delegate_ty, name);
        let call_args = args
            .iter()
            .copied()
            .map(CallArgKind::Typed)
            .collect::<Vec<_>>();
        let select_kind = |kind| {
            // Context parameters exclude a candidate here for the same reason as in
            // `select_delegate_operator`: the generated accessor has no scope to fill one from.
            let diagnostic_candidates = callables
                .functions()
                .iter()
                .filter(|candidate| candidate.kind == kind)
                .cloned()
                .collect::<Vec<_>>();
            let overloads = diagnostic_candidates
                .iter()
                .filter(|candidate| {
                    is_usable_delegate_convention(candidate.flags.operator, candidate.context_count)
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
                crate::symbol_resolver::ReceiverFunctionSelection::Selected((
                    selected,
                    _,
                    ret,
                    applied_receiver,
                )) => OrdinaryDelegateSelection::Selected(selected, ret, applied_receiver),
                crate::symbol_resolver::ReceiverFunctionSelection::None => {
                    OrdinaryDelegateSelection::None(diagnostic_candidates)
                }
                crate::symbol_resolver::ReceiverFunctionSelection::Ambiguous(candidates) => {
                    OrdinaryDelegateSelection::Ambiguous(candidates)
                }
            }
        };
        let selected = match select_kind(crate::libraries::FnKind::Member) {
            OrdinaryDelegateSelection::Selected(selected, ret, applied_receiver) => {
                (selected, ret, applied_receiver)
            }
            OrdinaryDelegateSelection::Ambiguous(candidates) => {
                return if optional {
                    DelegateOperatorSelection::None
                } else {
                    DelegateOperatorSelection::Failure(DelegateOperatorFailure::AmbiguousOrdinary(
                        candidates,
                    ))
                };
            }
            OrdinaryDelegateSelection::None(members) => {
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
                    MemberExtensionSelection::DelegateConventions,
                );
                let (member_extension, excluded) = match member_extension {
                    MemberExtensionFunctionSelection::Selected(selected) => {
                        (Some(selected), Vec::new())
                    }
                    MemberExtensionFunctionSelection::None(candidates) => (None, candidates),
                    MemberExtensionFunctionSelection::Ambiguous(candidates) => {
                        return if optional {
                            DelegateOperatorSelection::None
                        } else {
                            DelegateOperatorSelection::Failure(
                                DelegateOperatorFailure::AmbiguousMemberExtensions(candidates),
                            )
                        };
                    }
                };
                if let Some(selected) = member_extension {
                    if !self.member_accessible(selected.visibility, selected.owner) {
                        return DelegateOperatorSelection::None;
                    }
                    let interface = resolver
                        .classifier(selected.owner)
                        .is_some_and(|shape| shape.is_interface());
                    // The DECLARED value parameters, un-erased: the current-module declaration's own
                    // signature where there is one, otherwise the callable's generic signature. Never
                    // `physical_params`, which is already a target's erasure of this.
                    let declared_params = match selected.stable_declaration {
                        Some(declaration) => self
                            .stable_callable_declared_shape(declaration)
                            .map(|(_, parameters, _)| parameters)
                            .expect("a selected source delegate convention must retain its shape"),
                        None => selected.declared_params.clone(),
                    };
                    return DelegateOperatorSelection::Selected(Box::new(
                        DelegateGetValueTarget::MemberExtension {
                            stable_declaration: selected.stable_declaration,
                            external_identity: selected.external_identity,
                            external_default_provider: selected.external_default_provider,
                            owner: selected.owner,
                            name: selected.physical_name,
                            extension_receiver: selected.extension_receiver,
                            dispatch_receiver: self
                                .implicit_receiver_selection(selected.dispatch_receiver),
                            context_count: selected.context_count,
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
                        },
                    ));
                }
                match select_kind(crate::libraries::FnKind::Extension) {
                    OrdinaryDelegateSelection::Selected(selected, ret, applied_receiver) => {
                        (selected, ret, applied_receiver)
                    }
                    OrdinaryDelegateSelection::None(extensions) => {
                        return if optional
                            || (members.is_empty() && excluded.is_empty() && extensions.is_empty())
                        {
                            DelegateOperatorSelection::None
                        } else {
                            DelegateOperatorSelection::Failure(
                                DelegateOperatorFailure::InapplicableCandidates {
                                    members,
                                    member_extensions: excluded,
                                    extensions,
                                },
                            )
                        };
                    }
                    OrdinaryDelegateSelection::Ambiguous(candidates) => {
                        return if optional {
                            DelegateOperatorSelection::None
                        } else {
                            DelegateOperatorSelection::Failure(
                                DelegateOperatorFailure::AmbiguousOrdinary(candidates),
                            )
                        };
                    }
                }
            }
        };
        let (selected, ret, applied_receiver) = selected;
        let (declared_receiver, declared_params, declared_ret) = match selected.stable_declaration {
            Some(declaration) => self
                .stable_callable_declared_shape(declaration)
                .expect("a selected source delegate convention must retain its shape"),
            None => {
                let receiver = match selected.semantic_receiver() {
                    Some(receiver) => receiver,
                    None => delegate_ty,
                };
                (
                    receiver,
                    selected.semantic_signature().params.clone(),
                    selected.semantic_signature().ret,
                )
            }
        };
        if selected.is_extension() {
            let stable_declaration = selected.stable_declaration;
            return match resolver
                .build_extension_callable(name, delegate_ty, args, &[], &selected)
                .map(Box::new)
                .map(|callable| DelegateGetValueTarget::Extension {
                    callable,
                    stable_declaration,
                }) {
                Some(target) => DelegateOperatorSelection::Selected(Box::new(target)),
                None => DelegateOperatorSelection::None,
            };
        }
        let Some(internal) = delegate_ty.obj_internal() else {
            return DelegateOperatorSelection::None;
        };
        let resolved =
            resolver.materialize_member_function(delegate_ty, &call_args, &[], *selected);
        let owner = match resolved.member.owner {
            Some(owner) => owner,
            None => internal,
        };
        let interface = resolved.member.is_interface()
            || resolver
                .classifier(owner)
                .is_some_and(|classifier| classifier.is_interface());
        DelegateOperatorSelection::Selected(Box::new(DelegateGetValueTarget::Member {
            applied_receiver,
            declared_receiver,
            declared_params,
            declared_ret,
            stable_declaration: resolved.member.stable_declaration,
            external_identity: resolved.member.external_identity,
            external_default_provider: resolved.member.external_default_provider,
            owner,
            name: match &resolved.member.physical_name {
                Some(name) => name.clone(),
                None => resolved.member.name.clone(),
            },
            params: resolved.member.params,
            ret,
            physical_params: resolved.physical_params,
            physical_ret: resolved.member.physical_ret,
            descriptor: resolved.member.descriptor,
            interface,
        }))
    }

    /// The DECLARED shape of a current-module convention: its extension receiver or dispatch
    /// owner, value parameters, and result, all as declared — type parameters stay type parameters.
    ///
    /// This is deliberately not `physical_params`/`physical_ret`, which are the ABI slots after
    /// erasure. A consumer that needs to know a value crosses into a `T` slot must be told `T`;
    /// what `T` costs physically is the target's answer, and a different target gives a different
    /// one.
    fn stable_callable_declared_shape(
        &self,
        declaration: crate::fir::DeclarationId,
    ) -> Option<(Ty, Vec<Ty>, Ty)> {
        let index = self.resolved_index?;
        let callable = index.callable_for_declaration(declaration)?;
        let receiver = match callable.shape.extension_receiver {
            Some(receiver) => receiver.get(),
            None => {
                let owner = index.declaration_anchor(declaration)?.owner?;
                let classifier = index.classifier_header(owner)?.classifier;
                let arguments = index
                    .classifier_type_arguments(owner)?
                    .iter()
                    .map(|parameter| {
                        let header = index.type_parameter_header(*parameter)?;
                        let name = index.type_parameter_semantic_name(*parameter)?;
                        let bound = match header.bounds.first() {
                            Some(bound) => bound.ty.get(),
                            None => Ty::nullable(Ty::obj("kotlin/Any")),
                        };
                        Some(Ty::ty_param(name, bound))
                    })
                    .collect::<Option<Vec<_>>>()?;
                Ty::obj_args_name(classifier, &arguments)
            }
        };
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

#[cfg(test)]
mod selection_sizes {
    use std::mem::size_of;

    /// Every delegate-convention rung returns one of these by value. Boxing the selected callable
    /// keeps each selection a few words.
    ///
    /// Exact sizes, not an upper bound: a regression here is a field silently moving back inline,
    /// and a loose bound would not catch it. Update the numbers deliberately if a payload changes.
    #[test]
    fn delegate_selections_carry_a_pointer_to_their_callable() {
        assert_eq!(size_of::<super::DelegateOperatorSelection>(), 72);
        assert_eq!(size_of::<super::OrdinaryDelegateSelection>(), 72);
        assert_eq!(size_of::<super::DelegateConventionSelection>(), 40);
    }
}
