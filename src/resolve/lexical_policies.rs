//! Policies an annotation opens for everything lexically inside what it annotates.
//!
//! `@Suppress("NAME")` silences named diagnostics, and an opt-in marker or `@OptIn(Marker::class)`
//! accepts that marker's requirement (kotlinc's `isExperimentalityAcceptableInContext`). Both hold
//! for a declaration, an executable statement or an expression, and both nest, so the checker keeps
//! one stack and truncates it when it leaves the annotated element.

use super::*;

/// One policy in force at the current checking position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LexicalPolicy {
    /// Diagnostics with this name are not reported.
    Suppress(String),
    /// An enclosing element is annotated with this classifier, or opts in to it with
    /// `@OptIn(classifier::class)`. Either accepts the opt-in requirement of a marker classifier.
    OptIn(TypeName),
}

impl Checker<'_> {
    pub(super) fn suppresses_diagnostic(&self, name: &str) -> bool {
        self.active_lexical_policies.iter().rev().any(
            |policy| matches!(policy, LexicalPolicy::Suppress(suppressed) if suppressed == name),
        )
    }

    pub(super) fn visibility_access_suppressed(&self) -> bool {
        self.suppresses_diagnostic("INVISIBLE_REFERENCE")
            || self.suppresses_diagnostic("INVISIBLE_MEMBER")
    }

    /// Whether an enclosing element accepts the opt-in requirement of `marker`.
    pub(super) fn lexically_opted_in(&self, marker: TypeName) -> bool {
        self.active_lexical_policies
            .iter()
            .any(|policy| *policy == LexicalPolicy::OptIn(marker))
    }

    /// Open the policies of a declaration's annotations. Returns the depth to truncate back to.
    pub(super) fn push_declaration_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        annotations: &[AnnotationRef],
        arguments: &[Vec<ExprId>],
    ) -> usize {
        let depth = self.active_lexical_policies.len();
        for (annotation, arguments) in annotations.iter().zip(arguments) {
            let strings = arguments
                .iter()
                .filter_map(|argument| self.file.const_string_value(*argument))
                .map(|value| value.to_lossy())
                .collect::<Vec<_>>();
            // A restricted pass may reach a declaration whose annotation syntax was released; its
            // class literals are then out of reach, as they are for the application check.
            let class_literals = if arguments
                .iter()
                .all(|argument| self.file.expr_span(*argument).is_some())
            {
                self.file.class_literal_spellings(arguments)
            } else {
                Vec::new()
            };
            self.push_annotation_policies(scope, annotation, &strings, &class_literals);
        }
        depth
    }

    /// Open the policies of the classifiers whose bodies declare `classifier`, outermost first. A
    /// nested classifier is published beside its owners, so it is checked outside their
    /// declaration policies unless they are reopened here.
    pub(super) fn push_classifier_owner_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        classifier: DeclId,
    ) -> usize {
        let depth = self.active_lexical_policies.len();
        let mut owners = Vec::new();
        let mut current = classifier;
        while let Some(owner) = self.file.hoisted_classifier_owner(current) {
            owners.push(owner);
            current = owner;
        }
        for owner in owners.into_iter().rev() {
            if let Decl::Class(class) = self.file.decl(owner) {
                let (annotations, arguments) =
                    (class.annotations.clone(), class.annotation_args.clone());
                self.push_declaration_policies(scope, &annotations, &arguments);
            }
        }
        depth
    }

    /// Open the policies of the annotations written on statement `statement`.
    pub(super) fn push_statement_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
    ) -> usize {
        let depth = self.active_lexical_policies.len();
        if let Some(annotations) = self.file.statement_annotations.get(&statement).cloned() {
            self.push_use_site_policies(scope, &annotations);
        }
        depth
    }

    /// Open the policies of the annotations written on expression `expression`.
    pub(super) fn push_expression_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
    ) -> usize {
        let depth = self.active_lexical_policies.len();
        if let Some(annotations) = self.file.expression_annotations.get(&expression).cloned() {
            self.push_use_site_policies(scope, &annotations);
        }
        depth
    }

    /// Open the policies of the file annotations (`@file:OptIn(Marker::class)`,
    /// `@file:Suppress("NAME")`) for the whole checking unit. A bounded unit reparses only its own
    /// declaration, so these travel from the file prefix with the unit.
    pub(super) fn push_file_policies(&mut self, scope: &CheckerScope<'_>) {
        let policies = self.file.file_annotation_policies.clone();
        self.push_use_site_policies(scope, &policies);
    }

    fn push_use_site_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        annotations: &[UseSiteAnnotation],
    ) {
        for annotation in annotations {
            self.push_annotation_policies(
                scope,
                &annotation.annotation,
                &annotation.strings,
                &annotation.class_literals,
            );
        }
    }

    fn push_annotation_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        strings: &[String],
        class_literals: &[String],
    ) {
        if self.push_opt_in_policies(scope, annotation, class_literals)
            == Some(type_name("kotlin/Suppress"))
        {
            self.active_lexical_policies
                .extend(strings.iter().cloned().map(LexicalPolicy::Suppress));
        }
    }

    /// Accept what one annotation opts in to: the annotation classifier itself, and the markers
    /// an `@OptIn` names. Returns the annotation's identity.
    fn push_opt_in_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        class_literals: &[String],
    ) -> Option<TypeName> {
        let identity = self.annotation_identity_in_scope(scope, annotation)?;
        self.active_lexical_policies
            .push(LexicalPolicy::OptIn(identity));
        if identity == type_name("kotlin/OptIn") {
            for spelling in class_literals {
                if let Some(marker) = self.select_classifier(scope, spelling).found() {
                    self.active_lexical_policies
                        .push(LexicalPolicy::OptIn(marker));
                }
            }
        }
        Some(identity)
    }

    pub(super) fn check_annotation_applications_in_declaration_scope(
        &mut self,
        scope: &CheckerScope<'_>,
        annotations: &[AnnotationRef],
        arguments: &[Vec<ExprId>],
    ) {
        let policy_depth = self.push_declaration_policies(scope, annotations, arguments);
        for (annotation, arguments) in annotations.iter().zip(arguments) {
            self.check_annotation_application(scope, annotation, arguments);
        }
        self.active_lexical_policies.truncate(policy_depth);
    }
}
