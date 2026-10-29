//! Kotlin source-level member visibility and receiver constraints.

use super::*;

mod diagnostics;

pub(super) use diagnostics::{
    member_callable_message, owner_display, parameter_display, static_callable_message,
};

/// Whether the current compilation module declares `classifier` as an ordinary classifier.
///
/// This deliberately reads the module provider alone, not the federated resolver: dependency
/// classifiers stay visible for diagnostics but must not acquire same-module `internal` access.
/// The declaration facet is authoritative even while a streamed local classifier's full shape is
/// deferred, and its explicit `Ordinary` tag prevents a source typealias from claiming its target.
pub(super) fn module_declares_classifier(module: &dyn SymbolSource, classifier: TypeName) -> bool {
    let (namespace, name) = crate::symbol_source::SymbolNamespace::classifier_key(classifier);
    matches!(
        module.symbols(namespace, name).classifier_declaration.as_ref(),
        Some(crate::libraries::ClassifierDeclaration::Ordinary(declared))
            if *declared == classifier
    )
}

/// Facts a use site supplies to the one source-level visibility operation. Checking and signature
/// inference both call that operation; neither keeps a second copy of the rules.
pub(super) trait SourceMemberSite {
    fn visibility_suppressed(&self) -> bool;
    /// The use site's source package. `None` means the authoritative source record is absent;
    /// that is not the root package, and it does not grant package-private access.
    fn source_package(&self) -> Option<TypeName>;
    fn module_owns(&self, owner: TypeName) -> bool;
    fn internal_friend(
        &self,
        owner: TypeName,
        callable: Option<crate::fir::ExternalCallableId>,
    ) -> bool;
    fn access_classes(&self) -> Vec<TypeName>;
    fn protected_classes(&self) -> Vec<TypeName>;
    fn companion_of(&self, enclosing: TypeName) -> Option<TypeName>;
    fn is_subtype(&self, subclass: TypeName, superclass: TypeName) -> bool;
    fn receiver_assignable(&self, receiver: Ty, access_classifier: TypeName) -> bool;
}

/// Whether a member of `owner` with visibility `vis` is accessible from `site`. `internal` is
/// accessible when this module declares the owner or a friend module does. `private` reaches the
/// declaring class, classes lexically nested inside it, and a class whose companion declares the
/// member. `protected` reaches those plus any subclass of `owner`. A top-level site with no
/// enclosing class cannot see a non-public member. Java package-private declarations are
/// accessible from their declaring package.
/// The member-access operation when the selected dependency declaration has a stable provider
/// identity. The identity, rather than the package or classifier spelling, determines friendship.
pub(super) fn site_callable_accessible(
    site: &impl SourceMemberSite,
    vis: Visibility,
    owner: TypeName,
    callable: Option<crate::fir::ExternalCallableId>,
) -> bool {
    if site.visibility_suppressed() {
        return true;
    }
    match vis {
        Visibility::Public => true,
        Visibility::Internal => site.module_owns(owner) || site.internal_friend(owner, callable),
        Visibility::PackagePrivate => site.source_package() == Some(owner.namespace()),
        Visibility::Private | Visibility::Protected => {
            // Access is lexical, so the enclosing chain is walked, not the receiver chain: a
            // nested class has no outer receiver, yet it sits inside its outer class's body and
            // can reach that class's private members, including its companion's. Reaching down
            // from an enclosing class is the companion's privilege alone.
            site.access_classes().into_iter().any(|enclosing| {
                let companion = site.companion_of(enclosing);
                let nested_in_owner = std::iter::successors(enclosing.nested_owner(), |current| {
                    current.nested_owner()
                })
                .any(|ancestor| ancestor == owner);
                enclosing == owner
                    || nested_in_owner
                    || companion == Some(owner)
                    || (vis == Visibility::Protected
                        && (site.is_subtype(enclosing, owner)
                            || companion
                                .is_some_and(|companion| site.is_subtype(companion, owner))))
            })
        }
    }
}

/// [`site_callable_accessible`], plus the protected-receiver constraint.
pub(super) fn site_receiver_callable_accessible(
    site: &impl SourceMemberSite,
    vis: Visibility,
    owner: TypeName,
    receiver: Ty,
    callable: Option<crate::fir::ExternalCallableId>,
) -> bool {
    if !site_callable_accessible(site, vis, owner, callable) {
        return false;
    }
    vis != Visibility::Protected
        || site
            .protected_classes()
            .into_iter()
            .flat_map(|enclosing| std::iter::once(enclosing).chain(site.companion_of(enclosing)))
            .any(|access_classifier| {
                (access_classifier == owner || site.is_subtype(access_classifier, owner))
                    && site.receiver_assignable(receiver, access_classifier)
            })
}

impl SourceMemberSite for Checker<'_> {
    fn visibility_suppressed(&self) -> bool {
        self.visibility_access_suppressed()
    }

    fn source_package(&self) -> Option<TypeName> {
        Some(self.source_package_name())
    }

    fn module_owns(&self, owner: TypeName) -> bool {
        module_declares_classifier(&self.module, owner)
    }

    fn internal_friend(
        &self,
        owner: TypeName,
        callable: Option<crate::fir::ExternalCallableId>,
    ) -> bool {
        self.libraries
            .dependency_access_policy()
            .internal_callable_accessible(callable, owner)
    }

    fn access_classes(&self) -> Vec<TypeName> {
        self.access_context_class_names()
    }

    fn protected_classes(&self) -> Vec<TypeName> {
        self.lexical_source_class_names()
    }

    fn companion_of(&self, enclosing: TypeName) -> Option<TypeName> {
        self.resolver().classifier(enclosing).and_then(|class| {
            class
                .companion_object
                .as_ref()
                .map(|(_, companion)| *companion)
        })
    }

    fn is_subtype(&self, subclass: TypeName, superclass: TypeName) -> bool {
        self.obj_name_is_subtype(subclass, superclass)
    }

    fn receiver_assignable(&self, receiver: Ty, access_classifier: TypeName) -> bool {
        self.receiver_is_assignable(receiver, Ty::obj_name(access_classifier))
    }
}

impl<'a> Checker<'a> {
    pub(super) fn source_callable_visible(
        &self,
        candidate: &crate::libraries::FunctionInfo,
    ) -> bool {
        if self.suppresses_diagnostic("INVISIBLE_REFERENCE")
            || self.suppresses_diagnostic("INVISIBLE_MEMBER")
        {
            return true;
        }
        if !matches!(
            candidate.kind,
            crate::libraries::FnKind::TopLevel | crate::libraries::FnKind::Extension
        ) {
            return true;
        }
        crate::callable_access::source_callable_accessible(
            candidate.visibility,
            candidate.source_file,
            Some(self.file_index),
            || {
                self.libraries
                    .dependency_access_policy()
                    .internal_callable_accessible(
                        candidate.callable.external_identity,
                        candidate.callable.owner,
                    )
            },
        )
    }

    /// Whether a member function the call found on `receiver` is hidden from this site. Kotlin
    /// ranks a hidden member below every visible candidate: an applicable extension takes the
    /// call (`clone()` declared beside the protected `Object.clone`), and the member is reported
    /// as inaccessible only when nothing visible applies. `internal` ownership stays with the
    /// selected-member check.
    pub(super) fn member_hidden_from_site(
        &self,
        candidate: &crate::libraries::FunctionInfo,
        receiver: Option<Ty>,
    ) -> bool {
        let Some(receiver) = receiver else {
            return false;
        };
        matches!(
            candidate.visibility,
            Visibility::Private | Visibility::Protected | Visibility::PackagePrivate
        ) && !self.receiver_callable_accessible(
            candidate.visibility,
            candidate.callable.owner,
            crate::symbol_resolver::member_scope_receiver(receiver),
            candidate.callable.external_identity,
        )
    }

    /// An instance member the reference site cannot see does not shadow an applicable extension:
    /// `Col::clone` names the extension when `Object.clone` is protected. Without an extension the
    /// member stays, so the reference reports the member's visibility.
    pub(super) fn hidden_members_yield_to_extensions(
        &self,
        receiver: Ty,
        mut candidates: CallableRefCandidates,
    ) -> CallableRefCandidates {
        if self.drop_members_hidden_by_extensions(receiver, &mut candidates.overloads) {
            candidates.method = crate::symbol_resolver::select_instance_reference_from_functions(
                receiver,
                &candidates.overloads,
            );
        }
        candidates
    }

    /// Drop the members of `receiver` the site cannot see when a visible extension of the same
    /// name is a candidate too, so the extension is selected instead of the member's access error.
    /// Returns whether a member was dropped.
    pub(super) fn drop_members_hidden_by_extensions(
        &self,
        receiver: Ty,
        overloads: &mut Vec<crate::libraries::FunctionInfo>,
    ) -> bool {
        if !overloads
            .iter()
            .any(|candidate| candidate.is_extension() && self.source_callable_visible(candidate))
        {
            return false;
        }
        let before = overloads.len();
        overloads.retain(|candidate| {
            candidate.kind != crate::libraries::FnKind::Member
                || !self.member_hidden_from_site(candidate, Some(receiver))
        });
        overloads.len() != before
    }

    /// Drop the hidden members of `receiver` only when one of the visible extensions applies to
    /// this operator call. An extension of the same name that does not apply (`b + 1` against
    /// `B.plus(String)`) leaves the hidden member to report its visibility.
    pub(super) fn drop_members_hidden_by_applicable_extensions(
        &self,
        receiver: Ty,
        overloads: &mut Vec<crate::libraries::FunctionInfo>,
        call: OperatorCallShape<'_>,
    ) {
        let mut visible = overloads.clone();
        if !self.drop_members_hidden_by_extensions(receiver, &mut visible) {
            return;
        }
        let extensions = crate::libraries::Callables::Functions(crate::libraries::FunctionSet {
            overloads: visible
                .iter()
                .filter(|candidate| candidate.is_extension())
                .cloned()
                .collect(),
        });
        let resolver = self.resolver();
        let applies = match call {
            OperatorCallShape::Call { name, args } => resolver
                .select_receiver_function_with_params(receiver, name, args, &[], &extensions)
                .is_some(),
            OperatorCallShape::IndexedGet { args } => matches!(
                resolver.select_receiver_indexed_get_function_with_params(
                    receiver,
                    "get",
                    args,
                    &[],
                    &extensions,
                ),
                crate::symbol_resolver::CandidateSelection::Selected(_)
            ),
            OperatorCallShape::IndexedSet { args } => matches!(
                resolver.select_receiver_indexed_set_function_with_params(
                    receiver,
                    "set",
                    args,
                    &[],
                    &extensions,
                ),
                crate::symbol_resolver::CandidateSelection::Selected(_)
            ),
        };
        if applies {
            *overloads = visible;
        }
    }

    /// Reject an inaccessible selected constructor after diagnosing its local inference result.
    /// Evidence from the rejected call is never published into the committed-call sidecar.
    pub(super) fn reject_ctor_access(
        &mut self,
        call: ExprId,
        owner: TypeName,
        classifier: &crate::libraries::LibraryType,
        member: &crate::libraries::LibraryMember,
        bindings: Option<&crate::symbol_resolver::GSigBinds>,
    ) -> bool {
        if self.callable_accessible(member.visibility, owner, member.external_identity)
            || self.suppresses_diagnostic("INVISIBLE_REFERENCE")
        {
            return false;
        }
        if let Some(bindings) = bindings {
            self.report_rejected_constructor_type_arguments(call, owner, classifier, bindings);
        }
        self.diags.error(
            self.call_callee_name_span(call),
            diagnostics::constructor_message(&self.fed_source(), owner, member),
        );
        true
    }

    /// Whether one already-selected dependency callable is visible from the current site.
    pub(super) fn callable_accessible(
        &self,
        vis: Visibility,
        owner: TypeName,
        callable: Option<crate::fir::ExternalCallableId>,
    ) -> bool {
        site_callable_accessible(self, vis, owner, callable)
    }

    pub(super) fn receiver_callable_accessible(
        &self,
        vis: Visibility,
        owner: TypeName,
        receiver: Ty,
        callable: Option<crate::fir::ExternalCallableId>,
    ) -> bool {
        site_receiver_callable_accessible(self, vis, owner, receiver, callable)
    }

    /// Property-only visibility exception controlled by Kotlin's
    /// `AllowAccessToProtectedFieldFromSuperCompanion` language feature. Despite that historical
    /// feature name, this is a semantic property access: FIR records the resolved property and the
    /// target backend alone decides whether it becomes a field load or an accessor call.
    pub(super) fn receiver_property_accessible(
        &self,
        visibility: Visibility,
        owner: TypeName,
        receiver: Ty,
        callable: Option<crate::fir::ExternalCallableId>,
    ) -> bool {
        if self.visibility_access_suppressed() {
            return true;
        }
        if self.receiver_callable_accessible(visibility, owner, receiver, callable) {
            return true;
        }
        if visibility != Visibility::Protected
            || !self.file.allow_protected_super_companion_property_access
            || receiver.non_null().obj_internal() != Some(owner)
        {
            return false;
        }
        let Some(superclass) = owner.nested_owner() else {
            return false;
        };
        let declared_companion = self
            .resolver()
            .classifier(superclass)
            .and_then(|class| {
                class
                    .companion_object
                    .as_ref()
                    .map(|(_, companion)| *companion)
            })
            .is_some_and(|companion| companion == owner);
        declared_companion
            && self
                .lexical_source_class_names()
                .into_iter()
                .any(|enclosing| {
                    enclosing != superclass && self.obj_name_is_subtype(enclosing, superclass)
                })
    }

    /// Emit kotlinc's access diagnostic when a member of `owner` with visibility `vis` is not
    /// reachable from the current site.
    pub(super) fn reject_if_inaccessible(
        &mut self,
        vis: Visibility,
        name: &str,
        owner: TypeName,
        span: Span,
    ) -> bool {
        self.reject_callable_if_inaccessible(vis, name, owner, None, span)
    }

    pub(super) fn reject_callable_if_inaccessible(
        &mut self,
        vis: Visibility,
        name: &str,
        owner: TypeName,
        callable: Option<crate::fir::ExternalCallableId>,
        span: Span,
    ) -> bool {
        if !self.callable_accessible(vis, owner, callable)
            && !self.suppresses_diagnostic("INVISIBLE_REFERENCE")
        {
            let kind = match vis {
                Visibility::Private => "private",
                Visibility::Protected => "protected",
                Visibility::Internal => "internal",
                Visibility::PackagePrivate => "package-private",
                Visibility::Public => "public",
            };
            self.diags.error(
                span,
                format!(
                    "cannot access '{name}': it is {kind} in '{}'",
                    owner.render()
                ),
            );
            true
        } else {
            false
        }
    }

    /// Report a selected member the site cannot see with kotlinc's complete declaration message
    /// (`cannot access 'fun get(i: Int): String': it is protected in 'B'.`). Returns whether the
    /// member was rejected.
    pub(super) fn reject_inaccessible_selected_member(
        &mut self,
        span: Span,
        selected: &crate::symbol_resolver::ResolvedMember,
    ) -> bool {
        let Some(owner) = selected.member.owner else {
            return false;
        };
        if self.receiver_callable_accessible(
            selected.member.visibility,
            owner,
            selected.receiver,
            selected.member.external_identity,
        ) || self.suppresses_diagnostic("INVISIBLE_REFERENCE")
        {
            return false;
        }
        let visibility = match selected.member.visibility {
            Visibility::Private => "private",
            Visibility::Internal => "internal",
            Visibility::PackagePrivate => "package-private",
            Visibility::Protected => "protected",
            Visibility::Public => "public",
        };
        let message = member_callable_message(
            &self.fed_source(),
            self.resolved_index,
            &selected.member,
            owner,
            visibility,
        );
        self.diags.error(span, message);
        true
    }

    /// Emit an access rejection for one already-selected semantic function declaration.
    pub(super) fn reject_function_if_inaccessible(
        &mut self,
        function: &crate::libraries::FunctionInfo,
        owner: TypeName,
        span: Span,
    ) -> bool {
        if self.callable_accessible(
            function.visibility,
            owner,
            function.callable.external_identity,
        ) || self.suppresses_diagnostic("INVISIBLE_REFERENCE")
        {
            return false;
        }
        let visibility = match function.visibility {
            Visibility::Private => "private",
            Visibility::Protected => "protected",
            Visibility::Internal => "internal",
            Visibility::PackagePrivate => "package-private",
            Visibility::Public => "public",
        };
        let source = self.fed_source();
        let message = diagnostics::function_callable_message(
            &source,
            self.resolved_index,
            function,
            owner,
            visibility,
        );
        self.diags.error(span, message);
        true
    }

    /// Render an already-established semantic property-access rejection. The caller owns receiver
    /// and lexical accessibility; the provider contributes only the declaration label used by the
    /// diagnostic (for example Java's `field name: Type!`).
    pub(super) fn report_inaccessible_property(
        &mut self,
        visibility: Visibility,
        name: &str,
        ty: Ty,
        owner: TypeName,
        property: Option<crate::fir::ExternalPropertyId>,
        span: Span,
    ) {
        if self.visibility_access_suppressed() {
            return;
        }
        let label = property.and_then(|property| {
            self.libraries
                .external_property_diagnostic_label(property, name, ty)
        });
        let Some(label) = label else {
            self.reject_if_inaccessible(visibility, name, owner, span);
            return;
        };
        let visibility = match visibility {
            Visibility::Private => "private",
            Visibility::Protected => "protected",
            Visibility::Internal => "internal",
            Visibility::PackagePrivate => "package-private",
            Visibility::Public => "public",
        };
        self.diags.error(
            span,
            format!(
                "cannot access '{label}': it is {visibility} in '{}'.",
                owner_display(owner),
            ),
        );
    }

    /// Whether a property reference at this receiver exposes its setter. An inaccessible setter,
    /// including a protected setter whose receiver is not the accessing class, is absent.
    pub(super) fn property_reference_exposes_setter(
        &self,
        property: &crate::symbol_resolver::ResolvedPropertyRef,
        receiver: Ty,
    ) -> bool {
        property.setter.is_some()
            && self.receiver_callable_accessible(
                property.setter_visibility,
                property.getter.owner,
                receiver,
                property
                    .setter
                    .as_ref()
                    .and_then(|setter| setter.external_identity),
            )
    }
}

#[cfg(test)]
mod tests {
    /// The production frontend's stable declaration index must prove that an internal member
    /// belongs to this compilation.
    #[test]
    fn production_checker_uses_module_identity_for_internal_members() {
        let source = "package first\n\
                      class Owner { internal fun hidden(): Int = 1 }\n\
                      fun use(owner: Owner): Int = owner.hidden()";
        let mut diagnostics = crate::diag::DiagSink::new();
        let _ = crate::frontend::analyze_source_standalone(source, &mut diagnostics);

        assert_eq!(
            diagnostics
                .diags
                .iter()
                .map(|diagnostic| diagnostic.msg.as_str())
                .collect::<Vec<_>>(),
            Vec::<&str>::new()
        );
    }

    struct PackageSite(Option<crate::types::TypeName>);

    impl super::SourceMemberSite for PackageSite {
        fn visibility_suppressed(&self) -> bool {
            false
        }
        fn source_package(&self) -> Option<crate::types::TypeName> {
            self.0
        }
        fn module_owns(&self, _: crate::types::TypeName) -> bool {
            false
        }
        fn internal_friend(
            &self,
            _: crate::types::TypeName,
            _: Option<crate::fir::ExternalCallableId>,
        ) -> bool {
            false
        }
        fn access_classes(&self) -> Vec<crate::types::TypeName> {
            Vec::new()
        }
        fn protected_classes(&self) -> Vec<crate::types::TypeName> {
            Vec::new()
        }
        fn companion_of(&self, _: crate::types::TypeName) -> Option<crate::types::TypeName> {
            None
        }
        fn is_subtype(&self, _: crate::types::TypeName, _: crate::types::TypeName) -> bool {
            false
        }
        fn receiver_assignable(&self, _: crate::types::Ty, _: crate::types::TypeName) -> bool {
            false
        }
    }

    #[test]
    fn a_missing_source_record_does_not_grant_root_package_access() {
        let owner = crate::types::type_name("RootClass");
        assert_eq!(owner.namespace(), crate::types::TypeName::ROOT);
        assert!(!super::site_callable_accessible(
            &PackageSite(None),
            crate::types::Visibility::PackagePrivate,
            owner,
            None,
        ));
        assert!(super::site_callable_accessible(
            &PackageSite(Some(crate::types::TypeName::ROOT)),
            crate::types::Visibility::PackagePrivate,
            owner,
            None,
        ));
    }
}

/// The operator convention a hidden member competes for, so extension applicability is checked
/// with the same selection the call itself uses.
#[derive(Clone, Copy)]
pub(super) enum OperatorCallShape<'a> {
    Call {
        name: &'a str,
        args: &'a [crate::symbol_resolver::CallArgKind],
    },
    IndexedGet {
        args: &'a [crate::symbol_resolver::CallArgKind],
    },
    IndexedSet {
        args: &'a [crate::symbol_resolver::CallArgKind],
    },
}
