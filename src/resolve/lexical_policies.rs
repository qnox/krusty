//! The lexical policy stack: the policies the annotations around the current checking position
//! open (see [`crate::lexical_policy`]).
//!
//! Policies hold for a declaration, an executable statement or an expression, and nest, so the
//! checker keeps one stack and truncates it when it leaves the annotated element. Each application's
//! policies are resolved once, from the application checked in its owning scope, and every later
//! push of that application consumes the published fact.

use super::*;
pub(crate) use crate::lexical_policy::LexicalPolicy;

/// Resolve each source's file annotation policies once, while Pass 1 still holds the file
/// annotations' syntax, and publish them for every later checking unit of that source. The
/// applications' diagnostics belong to the unit that checks them, so this resolution reports none.
pub(crate) fn publish_file_lexical_policies(
    files: &[File],
    symbols: &SymbolTable,
    index: &mut crate::fir::ResolvedModuleIndex,
) {
    for (file_index, file) in files.iter().enumerate() {
        if file.file_annotations.is_empty() {
            continue;
        }
        let mut diagnostics = DiagSink::new();
        let policies = make_checker_with_index(
            file,
            file_index as u32,
            Some(files),
            symbols,
            None,
            None,
            None,
            &mut diagnostics,
        )
        .file_annotation_policies(&CheckerScope::root());
        index.publish_file_lexical_policies(
            crate::fir::SourceFileId::from_raw(file_index as u32),
            policies,
        );
    }
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
            .contains(&LexicalPolicy::OptIn(marker))
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
            let policies = self.application_policies(scope, annotation, arguments);
            self.active_lexical_policies
                .extend(policies.iter().cloned());
        }
        depth
    }

    /// The policies one annotation application opens, resolved once per application. The
    /// application is checked in `scope`, its owning scope, and its policies come from that check:
    /// the identity it bound and the classes and strings its arguments folded to. The check's own
    /// diagnostics belong to the element's ordinary application check, which runs inside these
    /// policies, so they are not reported here.
    fn application_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        annotation: &AnnotationRef,
        arguments: &[ExprId],
    ) -> std::rc::Rc<[LexicalPolicy]> {
        let key = (annotation.span.lo, annotation.span.hi);
        if let Some(policies) = self.lexical_policy_facts.get(&key) {
            return policies.clone();
        }
        let diagnostics = self.diags.diags.len();
        let identity = self.check_annotation_application(scope, annotation, arguments);
        self.diags.diags.truncate(diagnostics);
        let policies: std::rc::Rc<[LexicalPolicy]> = identity
            .map(|identity| {
                let values = self
                    .applied_annotations
                    .get(&key)
                    .map_or(&[][..], |applied| applied.values.as_slice());
                LexicalPolicy::of_application(identity, values)
            })
            .unwrap_or_default()
            .into();
        self.lexical_policy_facts.insert(key, policies.clone());
        policies
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

    /// Open the policies of the annotations written on statement `statement`, and check those
    /// applications inside them.
    pub(super) fn push_statement_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        statement: StmtId,
    ) -> usize {
        let depth = self.active_lexical_policies.len();
        if let Some(annotations) = self.file.statement_annotations.get(&statement).cloned() {
            self.open_use_site_policies(scope, &annotations);
        }
        depth
    }

    /// Open the policies of the annotations written on expression `expression`, and check those
    /// applications inside them.
    pub(super) fn push_expression_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
    ) -> usize {
        let depth = self.active_lexical_policies.len();
        if let Some(annotations) = self.file.expression_annotations.get(&expression).cloned() {
            self.open_use_site_policies(scope, &annotations);
        }
        depth
    }

    /// Open the policies of the file annotations (`@file:OptIn(Marker::class)`,
    /// `@file:Suppress("NAME")`) for the whole checking unit. A bounded unit reparses only its own
    /// declaration, so it seeds them from the fact Pass 1 published for its source.
    pub(super) fn push_file_policies(&mut self, policies: &[LexicalPolicy]) {
        self.active_lexical_policies
            .extend(policies.iter().cloned());
    }

    /// The policies of every file annotation, resolved in the file scope.
    pub(super) fn file_annotation_policies(
        &mut self,
        scope: &CheckerScope<'_>,
    ) -> Vec<LexicalPolicy> {
        let applications = self.file.file_annotations.clone();
        applications
            .iter()
            .flat_map(|(annotation, arguments)| {
                self.application_policies(scope, annotation, arguments)
                    .to_vec()
            })
            .collect()
    }

    fn open_use_site_policies(
        &mut self,
        scope: &CheckerScope<'_>,
        annotations: &[UseSiteAnnotation],
    ) {
        for annotation in annotations {
            let policies =
                self.application_policies(scope, &annotation.annotation, &annotation.arguments);
            self.active_lexical_policies
                .extend(policies.iter().cloned());
        }
        for annotation in annotations {
            self.check_annotation_application(scope, &annotation.annotation, &annotation.arguments);
        }
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
