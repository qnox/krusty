//! Opt-in requirements: kotlinc's `FirOptInUsageAccessChecker`, `FirOptInUsageQualifierChecker` and
//! `FirOptInUsageTypeRefChecker`.
//!
//! A declaration annotated with a marker class, one that is itself annotated `@RequiresOptIn`,
//! may be used only where that requirement is accepted: by `-opt-in=<marker>`, or by an enclosing
//! element annotated with the marker or with `@OptIn(Marker::class)` (see [`super::lexical_policies`]).
//! Each use is reported at the reference: the called or read name, the constructed classifier, the
//! qualifier, or the written type.
//!
//! A classifier's requirements include those of the classifiers it is nested in. A constructor
//! carries its classifier's; any other member carries only its own, since the dispatch receiver
//! that reaches it is checked as a use of its own.

use super::*;

/// What using one declaration requires (kotlinc's `Experimentality`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct OptInRequirement {
    marker: TypeName,
    warning: bool,
    message: Option<String>,
}

impl Checker<'_> {
    /// Report the opt-in requirements of the declaration expression `expression` uses, when its
    /// position does not accept them.
    pub(super) fn check_expression_opt_in(&mut self, scope: &CheckerScope<'_>, expression: ExprId) {
        if self.discover_anonymous_captures {
            return;
        }
        if let Some((qualifier, classifier)) = self.classifier_qualifier(expression) {
            let mut requirements = Vec::new();
            self.add_classifier_requirements(classifier, &mut requirements);
            self.report_opt_in_requirements(self.span(qualifier), &requirements);
        }
        if let Some((span, requirements)) = self.expression_opt_in_requirements(expression) {
            self.report_opt_in_requirements(span, &requirements);
        }
        if let Some(declared) = self.local_variable_read(scope, expression) {
            let mut requirements = Vec::new();
            self.add_type_requirements(declared, &mut requirements);
            self.report_opt_in_requirements(self.span(expression), &requirements);
        }
    }

    /// The declared type of the local variable or parameter `expression` reads. A variable's
    /// declared type is part of its signature, as a function's return type is.
    fn local_variable_read(&self, scope: &CheckerScope<'_>, expression: ExprId) -> Option<Ty> {
        let Expr::Name(name) = self.file.expr(expression) else {
            return None;
        };
        if self.expr_lowers.contains_key(&expression) {
            return None;
        }
        Some(self.lookup(scope, name)?.declared_ty)
    }

    /// The classifier qualifier `expression` is selected through (`Outer` in `Outer.Nested()`,
    /// `Obj` in `Obj.f()`), with the classifier the selection proves it denotes. A qualifier is a
    /// receiver that was never typed as a value, or an object's singleton.
    fn classifier_qualifier(&self, expression: ExprId) -> Option<(ExprId, TypeName)> {
        let (receiver, read) = match self.file.expr(expression) {
            Expr::Call { callee, .. } => match self.file.expr(*callee) {
                Expr::Member { receiver, .. } => (*receiver, false),
                _ => return None,
            },
            Expr::Member { receiver, .. } => (*receiver, true),
            _ => return None,
        };
        if !matches!(
            self.file.expr(receiver),
            Expr::Name(_) | Expr::Member { .. }
        ) {
            return None;
        }
        // An object qualifier is also its singleton value.
        if let Some(ExprLowering::SingletonValue(singleton)) = self.expr_lowers.get(&receiver) {
            return Some((receiver, singleton.classifier));
        }
        if self.expr_types[receiver.0 as usize] != Ty::Error {
            return None;
        }
        let classifier = if read {
            match self.expr_lowers.get(&expression)? {
                ExprLowering::AssociatedPropertyRead { owner, .. }
                | ExprLowering::ClassifierPropertyRead { owner, .. } => *owner,
                ExprLowering::SingletonValue(singleton) => singleton.classifier.nested_owner()?,
                _ => return None,
            }
        } else if let Some(constructor) = self.resolved_constructors.get(&expression) {
            constructor.owner().nested_owner()?
        } else {
            match self.resolved_calls.get(&expression)? {
                ResolvedCall::Member(selected) => {
                    selected.receiver.non_null().kotlin_class_internal()?
                }
                ResolvedCall::Companion(member) => member.owner?,
                _ => return None,
            }
        };
        Some((receiver, classifier))
    }

    /// Report the opt-in requirements of the classifier written type reference `reference` names.
    pub(super) fn check_type_reference_opt_in(&mut self, reference: &TypeRef, resolved: Ty) {
        if self.discover_anonymous_captures || reference.is_import() || reference.is_annotation() {
            return;
        }
        let mut requirements = Vec::new();
        self.add_type_requirements(resolved, &mut requirements);
        self.report_opt_in_requirements(reference.span, &requirements);
    }

    /// Report the opt-in requirements of the supertypes class `class` lists, as their types were
    /// published with the class header in Pass 1.
    pub(super) fn check_supertype_opt_in(&mut self, class: &ClassDecl, owner: Option<TypeName>) {
        if self.discover_anonymous_captures {
            return;
        }
        let Some(header) = owner
            .and_then(|owner| self.resolver().classifier(owner))
            .and_then(|classifier| classifier.stable_declaration)
            .zip(self.resolved_index)
            .and_then(|(declaration, index)| index.classifier_header(declaration))
        else {
            return;
        };
        let supertypes = header
            .declared_supertypes()
            .map(|supertype| supertype.get())
            .collect::<Vec<_>>();
        let mut references = class.supertypes.iter().collect::<Vec<_>>();
        let base = class.base_class.as_deref().map(|base| {
            base_class_type_ref(
                base,
                &class.base_type_args,
                class.base_class_span.unwrap_or(class.span),
            )
        });
        references.extend(base.as_ref());
        references.sort_by_key(|reference| reference.span.lo);
        // An implicit superclass has no reference to report at.
        if references.len() != supertypes.len() {
            return;
        }
        for (reference, supertype) in references.into_iter().zip(supertypes) {
            self.check_finalized_type_reference_opt_in(reference, supertype);
        }
    }

    /// Report a declaration type finalized in Pass 1, and each type argument written in it, as
    /// [`Self::check_type_reference_opt_in`] reports a reference the body check resolves.
    pub(super) fn check_finalized_type_reference_opt_in(
        &mut self,
        reference: &TypeRef,
        resolved: Ty,
    ) {
        self.check_type_reference_opt_in(reference, resolved);
        let arguments = resolved.non_null().type_args();
        for (argument, resolved) in reference.targs.iter().zip(arguments) {
            if !argument.is_star_projection() {
                self.check_finalized_type_reference_opt_in(argument, resolved.projection_read_ty());
            }
        }
    }

    /// The requirements of a classifier type and of its type arguments.
    fn add_type_requirements(&self, ty: Ty, requirements: &mut Vec<OptInRequirement>) {
        let ty = ty.non_null();
        let Some(classifier) = ty.kotlin_class_internal() else {
            return;
        };
        self.add_classifier_requirements(classifier, requirements);
        for argument in ty.type_args() {
            if !matches!(argument, Ty::StarProjection(_)) {
                self.add_type_requirements(argument.projection_read_ty(), requirements);
            }
        }
    }

    fn expression_opt_in_requirements(
        &self,
        expression: ExprId,
    ) -> Option<(Span, Vec<OptInRequirement>)> {
        let mut requirements = Vec::new();
        let span = match self.file.expr(expression) {
            Expr::Call { callee, .. } => {
                let span = self.callee_name_span(*callee);
                if let Some(constructor) = self.resolved_constructors.get(&expression) {
                    self.add_classifier_requirements(constructor.owner(), &mut requirements);
                    match constructor {
                        ResolvedConstructor::Source {
                            stable_declaration, ..
                        } => self.add_declaration_requirements(
                            *stable_declaration,
                            &[],
                            &mut requirements,
                        ),
                        ResolvedConstructor::Plain { member, .. }
                        | ResolvedConstructor::PlainSlots { member, .. } => self
                            .add_declaration_requirements(
                                member.stable_declaration,
                                &member.annotations,
                                &mut requirements,
                            ),
                        ResolvedConstructor::Synthetic { .. } => {}
                    }
                } else {
                    self.add_call_requirements(
                        self.resolved_calls.get(&expression)?,
                        &mut requirements,
                    );
                    self.add_call_type_argument_requirements(expression, &mut requirements);
                }
                span
            }
            Expr::SafeCall { name, args, .. } => {
                if args.is_some() {
                    self.add_call_requirements(
                        self.resolved_calls.get(&expression)?,
                        &mut requirements,
                    );
                    self.add_call_type_argument_requirements(expression, &mut requirements);
                } else {
                    self.add_read_requirements(
                        self.expr_lowers.get(&expression)?,
                        &mut requirements,
                    );
                }
                self.member_name_span(expression, name)
            }
            Expr::Member { name, .. } => {
                self.add_read_requirements(self.expr_lowers.get(&expression)?, &mut requirements);
                self.member_name_span(expression, name)
            }
            Expr::Name(_) => {
                self.add_read_requirements(self.expr_lowers.get(&expression)?, &mut requirements);
                self.span(expression)
            }
            Expr::CallableRef { name, .. } => {
                self.add_reference_requirements(
                    self.expr_lowers.get(&expression)?,
                    &mut requirements,
                );
                let span = self.span(expression);
                Span::new(span.hi.saturating_sub(name.len() as u32), span.hi)
            }
            _ => return None,
        };
        (!requirements.is_empty()).then_some((span, requirements))
    }

    /// A call's type arguments, written or inferred, are used where the callee is named.
    fn add_call_type_argument_requirements(
        &self,
        call: ExprId,
        requirements: &mut Vec<OptInRequirement>,
    ) {
        for &argument in self
            .resolved_call_type_args
            .get(&call)
            .into_iter()
            .flatten()
            .flatten()
        {
            self.add_type_requirements(argument, requirements);
        }
    }

    fn callee_name_span(&self, callee: ExprId) -> Span {
        match self.file.expr(callee) {
            Expr::Member { name, .. } => self.member_name_span(callee, name),
            _ => self.span(callee),
        }
    }

    fn add_call_requirements(&self, call: &ResolvedCall, requirements: &mut Vec<OptInRequirement>) {
        match call {
            ResolvedCall::Member(selected) => self.add_member_requirements(
                &selected.member,
                Some(selected.receiver),
                requirements,
            ),
            ResolvedCall::Companion(member) => {
                self.add_member_requirements(member, None, requirements)
            }
            ResolvedCall::TopLevel(call) => self.add_callable_requirements(
                &call.callable,
                call.stable_declaration,
                requirements,
            ),
            ResolvedCall::Extension(call) => self.add_callable_requirements(
                &call.callable,
                call.stable_declaration,
                requirements,
            ),
            ResolvedCall::MemberExtension {
                stable_declaration, ..
            } => self.add_declaration_requirements(*stable_declaration, &[], requirements),
            ResolvedCall::LocalFunction(call) => {
                if let Stmt::LocalFun(function) = self.file.stmt(call.stmt_id) {
                    let annotations = function
                        .annotations
                        .iter()
                        .filter_map(|annotation| {
                            self.applied_annotations
                                .get(&(annotation.span.lo, annotation.span.hi))
                                .map(|applied| applied.internal)
                        })
                        .collect::<Vec<_>>();
                    self.add_annotation_requirements(annotations, requirements);
                }
            }
        }
    }

    fn add_read_requirements(
        &self,
        lowering: &ExprLowering,
        requirements: &mut Vec<OptInRequirement>,
    ) {
        match lowering {
            ExprLowering::MemberPropertyRead {
                stable_declaration,
                accessor,
                ..
            } => self.add_declaration_requirements(
                *stable_declaration,
                accessor
                    .as_ref()
                    .map_or(&[][..], |accessor| &accessor.annotations),
                requirements,
            ),
            ExprLowering::TopLevelPropertyGet(access)
            | ExprLowering::ExtensionPropertyGet { access } => self.add_declaration_requirements(
                access.property.stable_declaration,
                &access.property.getter.annotations,
                requirements,
            ),
            ExprLowering::AssociatedPropertyRead {
                stable_declaration, ..
            }
            | ExprLowering::MemberExtensionPropertyRead {
                stable_declaration, ..
            } => self.add_declaration_requirements(*stable_declaration, &[], requirements),
            ExprLowering::SingletonValue(singleton) => {
                self.add_classifier_requirements(singleton.classifier, requirements)
            }
            _ => {}
        }
    }

    fn add_reference_requirements(
        &self,
        lowering: &ExprLowering,
        requirements: &mut Vec<OptInRequirement>,
    ) {
        match lowering {
            ExprLowering::ConstructorRef {
                internal,
                constructor,
                ..
            } => {
                self.add_classifier_requirements(*internal, requirements);
                self.add_declaration_requirements(
                    constructor.stable_declaration,
                    &constructor.annotations,
                    requirements,
                );
            }
            ExprLowering::TopLevelFunctionRef(reference) => self.add_declaration_requirements(
                reference.stable_declaration,
                &reference.target.annotations,
                requirements,
            ),
            ExprLowering::AdaptedRef {
                target,
                stable_declaration,
                ..
            } => self.add_declaration_requirements(
                *stable_declaration,
                &target.annotations,
                requirements,
            ),
            ExprLowering::CallableReference { target, .. }
            | ExprLowering::AdaptedCallableReference { target, .. } => match target {
                CallableReferenceTarget::Classifier(member)
                | CallableReferenceTarget::Member { member, .. } => self
                    .add_declaration_requirements(
                        member.stable_declaration,
                        &member.annotations,
                        requirements,
                    ),
                CallableReferenceTarget::Extension {
                    callable,
                    stable_declaration,
                } => self.add_declaration_requirements(
                    *stable_declaration,
                    &callable.annotations,
                    requirements,
                ),
                CallableReferenceTarget::Property(property) => self.add_declaration_requirements(
                    property.stable_declaration,
                    &property.getter.annotations,
                    requirements,
                ),
                CallableReferenceTarget::TopLevelProperty(property) => self
                    .add_declaration_requirements(
                        property.stable_declaration,
                        &property.getter.annotations,
                        requirements,
                    ),
                CallableReferenceTarget::ClassifierProperty { .. } => {}
            },
            _ => {}
        }
    }

    /// A package-level callable's requirements. A dependency's declared signature names its return
    /// type, its extension receiver, and its value parameters (contexts included).
    fn add_callable_requirements(
        &self,
        callable: &crate::libraries::LibraryCallable,
        stable_declaration: Option<crate::fir::DeclarationId>,
        requirements: &mut Vec<OptInRequirement>,
    ) {
        if self.add_source_or_provider_requirements(
            stable_declaration,
            &callable.annotations,
            requirements,
        ) {
            return;
        }
        for &ty in callable
            .declared_params
            .iter()
            .flatten()
            .chain(&callable.declared_ret)
        {
            self.add_type_requirements(ty, requirements);
        }
    }

    /// [`Self::add_callable_requirements`] for a member. A dependency member's value parameters are
    /// declared types only when no type argument of the dispatch receiver was substituted into
    /// them.
    fn add_member_requirements(
        &self,
        member: &crate::libraries::LibraryMember,
        dispatch: Option<Ty>,
        requirements: &mut Vec<OptInRequirement>,
    ) {
        if self.add_source_or_provider_requirements(
            member.stable_declaration,
            &member.annotations,
            requirements,
        ) {
            return;
        }
        if let Some(ret) = member.declared_ret {
            self.add_type_requirements(ret, requirements);
        }
        let parameters = match &member.generic_sig {
            Some(signature) => signature.receiver.iter().chain(&signature.params),
            None if dispatch.is_none_or(|receiver| receiver.non_null().type_args().is_empty()) => {
                None.iter().chain(&member.params)
            }
            None => return,
        };
        for &ty in parameters {
            self.add_type_requirements(ty, requirements);
        }
    }

    /// A declaration's own requirements and those of its declared signature types, when it is one of
    /// this compilation's declarations: from its published annotations and signature. Returns
    /// whether it was; a dependency declaration contributes only the annotations its provider
    /// recorded, and its caller adds the signature the provider describes.
    fn add_declaration_requirements(
        &self,
        stable_declaration: Option<crate::fir::DeclarationId>,
        provider_annotations: &[TypeName],
        requirements: &mut Vec<OptInRequirement>,
    ) {
        self.add_source_or_provider_requirements(
            stable_declaration,
            provider_annotations,
            requirements,
        );
    }

    fn add_source_or_provider_requirements(
        &self,
        stable_declaration: Option<crate::fir::DeclarationId>,
        provider_annotations: &[TypeName],
        requirements: &mut Vec<OptInRequirement>,
    ) -> bool {
        match stable_declaration.zip(self.resolved_index) {
            Some((declaration, index)) => {
                self.add_annotation_requirements(
                    index.declaration_annotations(declaration).iter().copied(),
                    requirements,
                );
                if let Some(signature) = index.signature(declaration) {
                    for ty in signature.parameters.iter().chain([&signature.result]) {
                        self.add_type_requirements(ty.get(), requirements);
                    }
                }
                true
            }
            None => {
                self.add_annotation_requirements(
                    provider_annotations.iter().copied(),
                    requirements,
                );
                false
            }
        }
    }

    /// A classifier's requirements, then those of each classifier it is nested in.
    fn add_classifier_requirements(
        &self,
        classifier: TypeName,
        requirements: &mut Vec<OptInRequirement>,
    ) {
        let mut current = Some(classifier);
        while let Some(classifier) = current {
            self.add_annotation_requirements(
                self.classifier_applied_annotations(classifier)
                    .iter()
                    .map(|annotation| annotation.annotation),
                requirements,
            );
            current = classifier.nested_owner();
        }
    }

    fn add_annotation_requirements(
        &self,
        annotations: impl IntoIterator<Item = TypeName>,
        requirements: &mut Vec<OptInRequirement>,
    ) {
        for annotation in annotations {
            if let Some(requirement) = self.opt_in_requirement(annotation) {
                if !requirements.contains(&requirement) {
                    requirements.push(requirement);
                }
            }
        }
    }

    /// The requirement an annotation class states with `@RequiresOptIn`, when it is a marker.
    fn opt_in_requirement(&self, annotation: TypeName) -> Option<OptInRequirement> {
        let requires = self
            .classifier_applied_annotations(annotation)
            .into_iter()
            .find(|applied| applied.annotation == type_name("kotlin/RequiresOptIn"))?;
        let mut requirement = OptInRequirement {
            marker: annotation,
            warning: false,
            message: None,
        };
        for (name, value) in &requires.arguments {
            match (name.as_str(), value) {
                ("level", crate::types::AnnotationValue::Enum(_, entry)) => {
                    requirement.warning = entry == "WARNING";
                }
                ("message", crate::types::AnnotationValue::String(message)) => {
                    requirement.message = Some(message.to_lossy());
                }
                _ => {}
            }
        }
        Some(requirement)
    }

    /// A classifier's annotations with their arguments: a current-module classifier's as its
    /// declaration published them, a dependency's as its provider recorded them.
    fn classifier_applied_annotations(
        &self,
        classifier: TypeName,
    ) -> Vec<crate::types::ResolvedAnnotation> {
        let Some(declaration) = self.resolver().classifier(classifier) else {
            return Vec::new();
        };
        match declaration.stable_declaration.zip(self.resolved_index) {
            Some((stable, index)) => index.declaration_applied_annotations(stable).to_vec(),
            None => declaration.annotations.clone(),
        }
    }

    fn report_opt_in_requirements(&mut self, span: Span, requirements: &[OptInRequirement]) {
        for requirement in requirements {
            let marker = kotlin_qualified_name(requirement.marker);
            // kotlinc's factories: `OPT_IN_USAGE` warns and `OPT_IN_USAGE_ERROR` rejects.
            let diagnostic = if requirement.warning {
                "OPT_IN_USAGE"
            } else {
                "OPT_IN_USAGE_ERROR"
            };
            if self.lexically_opted_in(requirement.marker)
                || self.file.opted_in_markers.contains(&marker)
                || self.suppresses_diagnostic(diagnostic)
            {
                continue;
            }
            let message =
                match requirement
                    .message
                    .as_deref()
                    .filter(|message| !message.trim().is_empty())
                {
                    Some(message) => lowercase_first(message),
                    None => format!(
                    "this declaration needs opt-in. Its usage {} be marked with '@{marker}' or \
                     '@OptIn({marker}::class)'",
                    if requirement.warning { "should" } else { "must" },
                ),
                };
            if requirement.warning {
                self.diags.warning(span, message);
            } else {
                self.diags.error(span, message);
            }
        }
    }
}

/// The dotted Kotlin qualified name kotlinc prints for a classifier (`ClassId.asFqNameString`),
/// which is also the spelling `-opt-in` takes.
fn kotlin_qualified_name(classifier: TypeName) -> String {
    let mut nested = Vec::new();
    let mut top_level = classifier;
    while let Some(owner) = top_level.nested_owner() {
        nested.push(top_level.nested_segment_ref());
        top_level = owner;
    }
    let mut name = top_level.render().replace('/', ".");
    for segment in nested.iter().rev() {
        name.push('.');
        name.push_str(segment);
    }
    name
}

/// kotlinc's renderer starts every diagnostic message in lower case, a custom one included.
fn lowercase_first(message: &str) -> String {
    let mut characters = message.chars();
    match characters.next() {
        Some(first) => first.to_lowercase().chain(characters).collect(),
        None => String::new(),
    }
}
