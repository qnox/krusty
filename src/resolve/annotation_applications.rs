//! Checking one annotation APPLICATION — its arguments, against the annotation class it names.
//!
//! The applications themselves were resolved and validated during header collection. What is left
//! here is the argument expressions, and whether this pass can still see them at all: a RESTRICTED
//! pass re-enters a declaration it did not select purely to rebuild that declaration's scopes, and
//! the annotation syntax of the members it skipped has already been released. Which passes those
//! are is an explicit declaration, not something inferred from the data a pass happens to carry —
//! see [`super::source_fragment`].

use super::*;

#[derive(Default)]
pub(super) struct CheckedAnnotationApplications {
    checked: HashMap<(u32, u32), crate::types::AppliedAnnotation>,
    discarded: crate::fx_hash::HashSet<(u32, u32)>,
    suppressed_source_property_applications: crate::fx_hash::HashSet<(u32, u32)>,
    /// The folded default of each element of a source annotation class that declares one, by
    /// class, in declaration order.
    element_defaults: HashMap<TypeName, Vec<(String, crate::types::AnnotationValue)>>,
}

impl CheckedAnnotationApplications {
    pub(super) fn get(&self, key: &(u32, u32)) -> Option<&crate::types::AppliedAnnotation> {
        self.checked.get(key)
    }

    fn discard(&mut self, annotation: &AnnotationRef) {
        let key = (annotation.span.lo, annotation.span.hi);
        self.checked.remove(&key);
        self.suppressed_source_property_applications.remove(&key);
        self.discarded.insert(key);
    }

    pub(super) fn record(&mut self, key: (u32, u32), applied: crate::types::AppliedAnnotation) {
        self.discarded.remove(&key);
        self.checked.insert(key, applied);
    }

    pub(super) fn record_element_defaults(
        &mut self,
        class: TypeName,
        defaults: Vec<(String, crate::types::AnnotationValue)>,
    ) {
        self.element_defaults.insert(class, defaults);
    }

    fn retain_suppressed_source_property_application(&mut self, annotation: &AnnotationRef) {
        self.suppressed_source_property_applications
            .insert((annotation.span.lo, annotation.span.hi));
    }

    fn was_discarded(&self, annotation: &AnnotationRef) -> bool {
        self.discarded
            .contains(&(annotation.span.lo, annotation.span.hi))
    }

    fn retains_suppressed_source_property_application(&self, annotation: &AnnotationRef) -> bool {
        self.suppressed_source_property_applications
            .contains(&(annotation.span.lo, annotation.span.hi))
    }
}

impl TypeInfo {
    pub(crate) fn discarded_annotation_application(&self, annotation: &AnnotationRef) -> bool {
        self.applied_annotations.was_discarded(annotation)
    }

    pub fn applied_annotation(
        &self,
        annotation: &AnnotationRef,
    ) -> Option<&crate::types::AppliedAnnotation> {
        self.applied_annotations
            .get(&(annotation.span.lo, annotation.span.hi))
    }

    /// The folded element defaults of the source annotation class `class`, when this pass folded
    /// them: the declaration-annotation fragment does, for every annotation class it selects.
    pub(crate) fn annotation_element_defaults(
        &self,
        class: TypeName,
    ) -> Option<&[(String, crate::types::AnnotationValue)]> {
        self.applied_annotations
            .element_defaults
            .get(&class)
            .map(Vec::as_slice)
    }

    pub(crate) fn retains_suppressed_source_property_application(
        &self,
        annotation: &AnnotationRef,
    ) -> bool {
        self.applied_annotations
            .retains_suppressed_source_property_application(annotation)
    }
}

/// The declaration that owns a property, which decides the targets its annotations may name.
#[derive(Clone, Copy)]
pub(super) enum PropertyAnnotationOwner {
    File,
    Classifier { owner_is_interface: bool },
}

/// One application bound in its scope, before any host check.
enum FoldedApplication {
    /// The application bound but cannot be applied (an optional expectation outside common
    /// sources, or a class with no element declaration); its diagnostic is already reported.
    Rejected(TypeName),
    /// The bound identity and, when every argument folded, the application's values.
    Arguments(TypeName, Option<crate::types::AppliedAnnotation>),
}

#[derive(Clone, Copy)]
enum AnnotationApplicationHost {
    Unspecified,
    Property {
        declaration_target: &'static str,
        field_unavailable_target: &'static str,
        has_backing_field: bool,
        mutable: bool,
        constructor_parameter: bool,
    },
    NonProperty {
        declaration_target: &'static str,
        constructor_parameter: bool,
        /// A declaration no annotation target denotes (an initializer): an annotation that allows
        /// an explicit use-site target is still wrong on it.
        targetless: bool,
    },
}

impl Checker<'_> {
    fn discard_annotation_application(&mut self, annotation: &AnnotationRef) {
        self.applied_annotations.discard(annotation);
    }

    /// Check one local function and its value-parameter applications while leaving the function's
    /// declaration policies active for its signature and body.
    pub(super) fn check_local_function_annotation_applications(
        &mut self,
        scope: &CheckerScope<'_>,
        function: &FunDecl,
    ) -> usize {
        let policy_depth =
            self.push_declaration_policies(scope, &function.annotations, &function.annotation_args);
        self.check_jvm_static_function(
            scope,
            function,
            super::declaration_rules::JvmStaticHost::Elsewhere,
        );
        for (annotation, arguments) in function.annotations.iter().zip(&function.annotation_args) {
            self.check_non_property_annotation_application(
                scope, annotation, arguments, "function",
            );
        }
        for parameter in &function.params {
            for (annotation, arguments) in
                parameter.annotations.iter().zip(&parameter.annotation_args)
            {
                self.check_non_property_annotation_application(
                    scope,
                    annotation,
                    arguments,
                    "value parameter",
                );
            }
        }
        policy_depth
    }

    pub(super) fn check_type_parameter_annotation_application(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        arguments: &[ExprId],
    ) {
        self.check_non_property_annotation_application(
            scope,
            annotation,
            arguments,
            "type parameter",
        );
    }

    /// Check a top-level property's applications and leave its declaration policies active for the
    /// body check that follows. The caller truncates to the returned depth when that body closes.
    pub(super) fn check_top_level_property_annotation_applications(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &PropDecl,
    ) -> usize {
        self.check_property_annotation_applications(
            scope,
            property,
            PropertyAnnotationOwner::File,
            &|_| true,
        )
    }

    /// Check a member property's applications within its own declaration-policy scope. Interface
    /// ownership is an explicit host fact: a body-less interface property is implicitly abstract
    /// even though its parser node has no written `abstract` modifier.
    pub(super) fn check_member_property_annotation_applications(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &PropDecl,
        owner_is_interface: bool,
    ) {
        let policy_depth = self.check_property_annotation_applications(
            scope,
            property,
            PropertyAnnotationOwner::Classifier { owner_is_interface },
            &|_| true,
        );
        self.active_lexical_policies.truncate(policy_depth);
    }

    /// Check the applications of one property against both the resolved annotation declaration and
    /// the semantic storage shape of their host declaration. Only the applications `include`
    /// selects are checked; the returned depth closes the property's declaration policies.
    pub(super) fn check_property_annotation_applications(
        &mut self,
        scope: &CheckerScope<'_>,
        property: &PropDecl,
        owner: PropertyAnnotationOwner,
        include: &dyn Fn(&AnnotationRef) -> bool,
    ) -> usize {
        let (fieldless_description, field_description, owner_forbids_fields) = match owner {
            PropertyAnnotationOwner::File => (
                "top level property without backing field or delegate",
                "top level property with backing field",
                false,
            ),
            PropertyAnnotationOwner::Classifier { owner_is_interface } => (
                "member property without backing field or delegate",
                "member property with backing field",
                owner_is_interface,
            ),
        };
        let policy_depth = self.push_selected_declaration_policies(
            scope,
            &property.annotations,
            &property.annotation_args,
            &|annotation| {
                include(annotation)
                    && super::lexical_policies::property_declaration_site(annotation)
            },
        );
        // The parser hoists a `companion { ... }` property as a classifier-associated extension,
        // but an initialized/lateinit/explicit-field declaration still owns static storage on the
        // declaring classifier. Validate its `@field:` site from that semantic storage shape, not
        // from the parser-only receiver used to retain the association.
        let companion_block_storage = property.is_companion_block_member
            && (property.init.is_some()
                || property.is_lateinit
                || property.explicit_backing_field.is_some());
        let has_backing_field = !owner_forbids_fields
            && (crate::ast::has_backing_field(property) || companion_block_storage);
        for (annotation, arguments) in property.annotations.iter().zip(&property.annotation_args) {
            if !include(annotation) {
                continue;
            }
            self.check_annotation_application_for_host(
                scope,
                annotation,
                arguments,
                AnnotationApplicationHost::Property {
                    declaration_target: if has_backing_field {
                        field_description
                    } else {
                        fieldless_description
                    },
                    field_unavailable_target: fieldless_description,
                    has_backing_field,
                    mutable: property.is_var,
                    constructor_parameter: false,
                },
            );
        }
        policy_depth
    }

    /// Check a primary-constructor parameter's applications. A `val`/`var` parameter ordinarily
    /// owns a field; an annotation-class element is a property but its interface has no backing
    /// field. A plain parameter is a value parameter and cannot host an explicit `@field:`
    /// application either.
    pub(super) fn check_constructor_parameter_annotations(
        &mut self,
        scope: &CheckerScope<'_>,
        parameter: &crate::ast::PropParam,
        owner_is_annotation: bool,
    ) {
        self.check_selected_constructor_parameter_annotations(
            scope,
            parameter,
            owner_is_annotation,
            &|_| true,
        );
    }

    /// [`Self::check_constructor_parameter_annotations`] for only the applications `include`
    /// selects.
    pub(super) fn check_selected_constructor_parameter_annotations(
        &mut self,
        scope: &CheckerScope<'_>,
        parameter: &crate::ast::PropParam,
        owner_is_annotation: bool,
        include: &dyn Fn(&AnnotationRef) -> bool,
    ) {
        let policy_depth = self.push_selected_declaration_policies(
            scope,
            &parameter.annotations,
            &parameter.annotation_args,
            include,
        );
        for (annotation, arguments) in parameter.annotations.iter().zip(&parameter.annotation_args)
        {
            if !include(annotation) {
                continue;
            }
            let host = if parameter.is_property {
                AnnotationApplicationHost::Property {
                    declaration_target: "value parameter",
                    field_unavailable_target: if owner_is_annotation {
                        "member property without backing field or delegate"
                    } else {
                        "value parameter"
                    },
                    has_backing_field: !owner_is_annotation,
                    mutable: parameter.is_var,
                    constructor_parameter: true,
                }
            } else {
                AnnotationApplicationHost::NonProperty {
                    declaration_target: "value parameter",
                    constructor_parameter: true,
                    targetless: false,
                }
            };
            self.check_annotation_application_for_host(scope, annotation, arguments, host);
        }
        self.active_lexical_policies.truncate(policy_depth);
    }

    /// A field use-site on a declaration that is not a property is invalid independently of the
    /// annotation class's own target set.
    pub(super) fn check_non_property_annotation_application(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        arguments: &[ExprId],
        declaration_target: &'static str,
    ) -> Option<TypeName> {
        self.check_annotation_application_for_host(
            scope,
            annotation,
            arguments,
            AnnotationApplicationHost::NonProperty {
                declaration_target,
                constructor_parameter: false,
                targetless: false,
            },
        )
    }

    /// `@Volatile` is a property contract, not merely a JVM flag. Publishing it on an immutable
    /// property would ask the backend to emit the classfile-forbidden `FINAL | VOLATILE` pair.
    fn validate_immutable_volatile_property(
        &mut self,
        mutable: bool,
        annotation: &AnnotationRef,
        identity: TypeName,
    ) -> bool {
        if mutable || identity != crate::types::type_name("kotlin/jvm/Volatile") {
            return true;
        }
        if self.suppresses_diagnostic("VOLATILE_ON_VALUE") {
            // Suppression keeps this SOURCE application as a declaration fact even though its
            // annotation payload does not survive retention.
            self.applied_annotations
                .retain_suppressed_source_property_application(annotation);
            return true;
        }
        self.diags.error(
            Span::new(annotation.at, annotation.span.hi),
            "'@Volatile' annotation cannot be used on immutable properties.".to_string(),
        );
        self.discard_annotation_application(annotation);
        false
    }

    /// Check one application and publish its folded arguments. Returns the annotation identity the
    /// application binds, also when its arguments are rejected.
    pub(super) fn check_annotation_application(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        arguments: &[ExprId],
    ) -> Option<TypeName> {
        self.check_annotation_application_for_host(
            scope,
            annotation,
            arguments,
            AnnotationApplicationHost::Unspecified,
        )
    }

    /// The identity and folded values of an application written for a use-site of a property
    /// (`@get:`), for the lexical policies it opens over that site. The property's own host check
    /// validates and publishes the application; this one only binds and folds it.
    pub(super) fn use_site_application_values(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        arguments: &[ExprId],
    ) -> Option<(TypeName, Vec<(String, crate::types::AnnotationValue)>)> {
        match self.fold_annotation_application_in_scope(scope, annotation, arguments)? {
            FoldedApplication::Rejected(_) => None,
            FoldedApplication::Arguments(internal, applied) => Some((
                internal,
                applied.map(|applied| applied.values).unwrap_or_default(),
            )),
        }
    }

    /// Bind one application in `scope` and fold its arguments, without validating it against a
    /// declaration host or publishing it. The host check and the lexical policies an application
    /// opens both start from this.
    fn fold_annotation_application_in_scope(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        arguments: &[ExprId],
    ) -> Option<FoldedApplication> {
        if arguments
            .iter()
            .any(|argument| self.file.expr_span(*argument).is_none())
        {
            // Annotation applications were resolved and validated during header collection. A
            // RESTRICTED pass re-enters a declaration it did not select solely to recreate that
            // declaration's lexical type and value scopes, and the annotation expressions of the
            // members it skipped are outside the fragment it retains. Two passes are restricted
            // this way: Pass-1 default checking, and inline preparation, which selects only the
            // inline declarations it must expand. An UNRESTRICTED pass has every expression and
            // must never reach released syntax.
            assert!(
                self.fragment.may_observe_released_annotation_syntax(),
                "a complete pass reached released annotation syntax; fragment={:?}",
                self.fragment
            );
            return None;
        }
        let reference = self
            .file
            .annotation_type_references
            .get(&annotation.span.lo)
            .cloned()
            .unwrap_or_else(|| TypeRef {
                name: annotation.name.clone(),
                flags: TrFlags::default(),
                arg: None,
                targs: Vec::new(),
                span: annotation.span,
                fun_params: Box::new([]),
                fun_context_count: 0,
                qualifier_arguments: Default::default(),
            });
        let owner = if self.fragment.publishes_annotation_metadata() {
            // The selected declaration facet was bound before actualization. Apply its syntax in
            // this lexical scope without selecting the annotation spelling again.
            let binding = self
                .bound_annotation_identities
                .get(&(annotation.span.lo, annotation.span.hi))
                .copied()?;
            self.bound_annotation_owner(scope, binding, &reference)
        } else {
            self.type_ref_ty_reported(scope, &reference)
        };
        let internal = owner.kotlin_class_internal()?;
        if !self.file.is_common
            && self.is_optional_expectation_classifier(internal)
            && !self.suppresses_diagnostic("OPTIONAL_DECLARATION_USAGE_IN_NON_COMMON_SOURCE")
        {
            // kotlinc's OPTIONAL_DECLARATION_USAGE_IN_NON_COMMON_SOURCE, on the type reference.
            self.diags.error(
                annotation.span,
                "declaration annotated with '@OptionalExpectation' can only be used in common \
                 module sources."
                    .to_string(),
            );
            return Some(FoldedApplication::Rejected(internal));
        }
        let Some(shape) = self.applied_annotation_shape(owner) else {
            self.diags.error(
                annotation.span,
                "resolved annotation has no semantic element declaration".to_string(),
            );
            return Some(FoldedApplication::Rejected(internal));
        };
        let applied =
            if self.check_annotation_arguments(scope, annotation.span, &shape, arguments, None) {
                match self.fold_annotation_application(
                    owner,
                    arguments,
                    annotation.property_site,
                    written_short_name(&annotation.name),
                ) {
                    Some(applied) => Some(applied),
                    None => {
                        if !self.report_non_constant_annotation_arguments(arguments) {
                            self.diags.error(
                                annotation.span,
                                "annotation argument is not a supported compile-time constant"
                                    .to_string(),
                            );
                        }
                        None
                    }
                }
            } else {
                None
            };
        Some(FoldedApplication::Arguments(internal, applied))
    }

    /// Check an annotation application against the declaration host that owns it.
    fn check_annotation_application_for_host(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        arguments: &[ExprId],
        host: AnnotationApplicationHost,
    ) -> Option<TypeName> {
        let (internal, applied) =
            match self.fold_annotation_application_in_scope(scope, annotation, arguments)? {
                FoldedApplication::Rejected(internal) => {
                    let _ = self.validate_property_annotation_contract(annotation, internal, host);
                    return Some(internal);
                }
                FoldedApplication::Arguments(internal, applied) => (internal, applied),
            };
        // Argument failures precede host-contract failures, while a property contract precedes
        // the target diagnostic for the same annotation (kotlinc's diagnostic ledger order).
        let contract_valid = self.validate_property_annotation_contract(annotation, internal, host);
        if !self.validate_property_annotation_site(annotation, internal, host) {
            return Some(internal);
        }
        if contract_valid {
            if let Some(mut applied) = applied {
                if let AnnotationApplicationHost::Property {
                    constructor_parameter,
                    has_backing_field,
                    ..
                } = host
                {
                    applied.property_sites = Some(applied.targets.property_declaration_sites(
                        crate::types::PropertyAnnotationHost {
                            constructor_parameter,
                            has_backing_field,
                        },
                        self.file.property_annotation_default_target_mode,
                        annotation.property_site,
                    ));
                }
                let key = (annotation.span.lo, annotation.span.hi);
                self.applied_annotations.record(key, applied);
            }
        }
        Some(internal)
    }

    fn validate_property_annotation_contract(
        &mut self,
        annotation: &AnnotationRef,
        internal: TypeName,
        host: AnnotationApplicationHost,
    ) -> bool {
        if let AnnotationApplicationHost::Property { mutable, .. } = host {
            return self.validate_immutable_volatile_property(mutable, annotation, internal);
        }
        true
    }

    /// Validate an explicit use-site, and the implicit property site selected by the annotation's
    /// resolved target policy. Kotlin reports argument failures first; an invalid site is never
    /// published as a checked application merely because its values happened to fold.
    fn validate_property_annotation_site(
        &mut self,
        annotation: &AnnotationRef,
        internal: TypeName,
        host: AnnotationApplicationHost,
    ) -> bool {
        // A source annotation class can be checked before its declaration policies are published,
        // including while the metadata-publication fragment is being assembled. Bind the
        // application, but defer its use-site diagnostic until declaration-owned target facts are
        // available; applying the default target set here would publish a duplicate, false error.
        if (access_control::module_declares_classifier(&self.module, internal)
            || self.module.annotation_retention(internal).is_some())
            && self
                .module
                .annotation_targets()
                .diagnostic(internal)
                .is_none()
            && self
                .module
                .annotation_targets()
                .semantic(internal)
                .is_none()
        {
            return true;
        }
        let targets = self
            .diagnostic_annotation_targets(internal)
            .unwrap_or(crate::types::AnnotationTargets::DEFAULT);
        let (
            declaration_target,
            field_unavailable_target,
            is_property,
            has_backing_field,
            mutable,
            constructor_parameter,
        ) = match host {
            AnnotationApplicationHost::Property {
                declaration_target,
                field_unavailable_target,
                has_backing_field,
                mutable,
                constructor_parameter,
            } => (
                Some(declaration_target),
                Some(field_unavailable_target),
                true,
                has_backing_field,
                mutable,
                constructor_parameter,
            ),
            AnnotationApplicationHost::NonProperty {
                declaration_target,
                constructor_parameter,
                ..
            } => (
                Some(declaration_target),
                None,
                false,
                false,
                false,
                constructor_parameter,
            ),
            AnnotationApplicationHost::Unspecified => (None, None, false, false, false, false),
        };
        let site = annotation.property_site.or_else(|| match host {
            AnnotationApplicationHost::Property {
                has_backing_field: false,
                constructor_parameter,
                ..
            } => targets.property_declaration_site(constructor_parameter),
            _ => None,
        });
        let Some(site) = site else { return true };
        let wrong_target_suppression = if annotation.property_site.is_some() {
            "WRONG_ANNOTATION_TARGET_WITH_USE_SITE_TARGET"
        } else {
            "WRONG_ANNOTATION_TARGET"
        };
        let target = site.kotlin_target();
        let unavailable = match site {
            crate::types::PropertyAnnotationSite::ValueParameter if !constructor_parameter => {
                Some("'@param:' annotations can only be applied to primary constructor parameters.")
            }
            crate::types::PropertyAnnotationSite::Property
            | crate::types::PropertyAnnotationSite::Getter
            | crate::types::PropertyAnnotationSite::Setter
            | crate::types::PropertyAnnotationSite::SetterParameter
                if !is_property =>
            {
                Some(match site {
                    crate::types::PropertyAnnotationSite::Property => {
                        "'@property:' annotations can only be applied to property declarations."
                    }
                    crate::types::PropertyAnnotationSite::Getter => {
                        "'@get:' annotations can only be applied to property declarations."
                    }
                    crate::types::PropertyAnnotationSite::Setter => {
                        "'@set:' annotations can only be applied to property declarations."
                    }
                    crate::types::PropertyAnnotationSite::SetterParameter => {
                        "'@setparam:' annotations can only be applied to property declarations."
                    }
                    _ => unreachable!("matched a property-only annotation use-site"),
                })
            }
            crate::types::PropertyAnnotationSite::Setter if !mutable => {
                Some("'@set:' annotations can only be applied to mutable properties.")
            }
            crate::types::PropertyAnnotationSite::SetterParameter if !mutable => {
                Some("'@setparam:' annotations can only be applied to mutable properties.")
            }
            _ => None,
        };
        if let Some(message) = unavailable {
            self.diags.error(
                Span::new(annotation.at, annotation.span.hi),
                message.to_string(),
            );
        }
        let host_valid = unavailable.is_none()
            && (site != crate::types::PropertyAnnotationSite::Field || has_backing_field);
        let suppressed = self.suppresses_diagnostic(wrong_target_suppression);
        if host_valid && (targets.allows(target) || suppressed) {
            return true;
        }
        // kotlinc retains a suppressed `@field:` application on a non-property's actual
        // declaration host. A property without storage has no declaration capable of carrying it.
        if suppressed
            && site == crate::types::PropertyAnnotationSite::Field
            && matches!(host, AnnotationApplicationHost::NonProperty { .. })
        {
            return true;
        }
        let targetless = matches!(
            host,
            AnnotationApplicationHost::NonProperty {
                targetless: true,
                ..
            }
        );
        if suppressed || (unavailable.is_some() && targets.allows(target) && !targetless) {
            self.discard_annotation_application(annotation);
            return false;
        }
        let applicable = targets.diagnostic_list();
        let target_description =
            if site == crate::types::PropertyAnnotationSite::Field && !has_backing_field {
                field_unavailable_target
                    .or(declaration_target)
                    .unwrap_or_else(|| target.description())
            } else if host_valid {
                if site == crate::types::PropertyAnnotationSite::Field {
                    "backing field"
                } else {
                    target.description()
                }
            } else {
                declaration_target.unwrap_or_else(|| target.description())
            };
        let use_site = annotation.property_site.map_or_else(String::new, |site| {
            format!(" and use-site target '@{}'", site.keyword())
        });
        self.diags.error(
            Span::new(annotation.at, annotation.span.hi),
            format!(
                "this annotation is not applicable to target '{target_description}'{use_site}. \
                 Applicable targets: {applicable}"
            ),
        );
        self.discard_annotation_application(annotation);
        false
    }

    /// A classifier's own annotations are resolved where the classifier is declared: a nested
    /// classifier's annotations see the static rungs of the classifier that contains it (`@Dsl
    /// class Inner` naming a sibling `annotation class Dsl`), never its own nested declarations.
    pub(super) fn check_classifier_own_annotations(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
        owner: Option<TypeName>,
    ) {
        let container = self.enter_annotation_class_rung(owner.and_then(TypeName::nested_owner));
        for (annotation, arguments) in class.annotations.iter().zip(&class.annotation_args) {
            self.check_non_property_annotation_application(scope, annotation, arguments, "class");
        }
        self.exit_annotation_class_rung(container);
    }

    /// Open the lexical rung of `class` for annotation applications checked outside its body
    /// scope. A member's annotations (`@Ser(Nested::class) val p`, `@Tag(CONST) constructor()`)
    /// see the static rungs of the classifier that declares the member, as a classifier's own
    /// annotations see those of its container. Returns the rung this call opened, if the class
    /// was not already lexically enclosing.
    pub(super) fn enter_annotation_class_rung(
        &mut self,
        class: Option<TypeName>,
    ) -> Option<TypeName> {
        let rung = class.filter(|class| {
            self.fed_source().classifier(*class).is_some()
                && !self.lexical_source_class_names().contains(class)
        })?;
        self.lexical_class_context.insert(0, rung);
        Some(rung)
    }

    pub(super) fn exit_annotation_class_rung(&mut self, rung: Option<TypeName>) {
        if let Some(rung) = rung {
            let removed = self.lexical_class_context.remove(0);
            debug_assert_eq!(removed, rung);
        }
    }
}

/// The retention of an annotation classifier: the module's own normalized fact for a source
/// declaration, otherwise the provider's declared policy. `None` for a policy this compiler does not
/// know, which makes the application unsupported rather than silently retained.
pub(super) fn annotation_retention(
    module_retention: Option<crate::types::AnnotationRetention>,
    classifier: &crate::libraries::LibraryType,
) -> Option<crate::types::AnnotationRetention> {
    module_retention.or_else(|| {
        Some(match classifier.retention.as_deref() {
            Some("SOURCE") => crate::types::AnnotationRetention::Source,
            Some("BINARY" | "CLASS") => crate::types::AnnotationRetention::Binary,
            Some("RUNTIME") => crate::types::AnnotationRetention::Runtime,
            None => crate::types::AnnotationRetention::Default,
            Some(_) => return None,
        })
    })
}

/// The last segment of an annotation's written class reference, which names it in rendering.
fn written_short_name(spelling: &str) -> &str {
    spelling.rsplit('.').next().unwrap_or(spelling)
}

/// kotlinc's WRONG_ANNOTATION_TARGET message for an application on a declaration that
/// `target_description` names, given the annotation class's resolved targets.
pub(super) fn wrong_annotation_target_message(
    target_description: &str,
    targets: crate::types::AnnotationTargets,
) -> String {
    format!(
        "this annotation is not applicable to target '{target_description}'. Applicable targets: \
         {}",
        targets.diagnostic_list()
    )
}

impl Checker<'_> {
    /// Check the annotations of `class`'s secondary constructors.
    pub(super) fn check_secondary_constructor_annotations(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
    ) {
        for constructor in &class.secondary_ctors {
            self.check_non_property_annotation_applications_in_declaration_scope(
                scope,
                &constructor.annotations,
                &constructor.annotation_args,
                "constructor",
            );
        }
    }

    /// Check the annotations written before an `init` block, in the scope its body is checked in:
    /// they see the class's instance members, as a member function's do. An initializer is a declaration no
    /// annotation target denotes, so every application that resolves to an annotation class is a
    /// WRONG_ANNOTATION_TARGET unless suppressed; its arguments are checked like any other.
    pub(super) fn check_initializer_annotation_applications(
        &mut self,
        scope: &CheckerScope<'_>,
        block: crate::ast::ExprId,
    ) {
        let file = self.file;
        let Some(header) = file.init_blocks.get(&block) else {
            return;
        };
        let policy_depth =
            self.push_declaration_policies(scope, &header.annotations, &header.annotation_args);
        for (annotation, arguments) in header.annotations.iter().zip(&header.annotation_args) {
            let Some(classifier) = self.check_annotation_application_for_host(
                scope,
                annotation,
                arguments,
                AnnotationApplicationHost::NonProperty {
                    declaration_target: "initializer",
                    constructor_parameter: false,
                    targetless: true,
                },
            ) else {
                continue;
            };
            // An explicit use-site was reported by the site validation above, which knows the
            // initializer has no target.
            if annotation.property_site.is_some()
                || self.suppresses_diagnostic("WRONG_ANNOTATION_TARGET")
            {
                continue;
            }
            let targets = self
                .diagnostic_annotation_targets(classifier)
                .unwrap_or(crate::types::AnnotationTargets::DEFAULT);
            self.diags.error(
                Span::new(annotation.at, annotation.span.hi),
                wrong_annotation_target_message("initializer", targets),
            );
            self.discard_annotation_application(annotation);
        }
        self.active_lexical_policies.truncate(policy_depth);
    }
}
