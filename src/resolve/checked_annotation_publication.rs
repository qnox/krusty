//! Stable publication of fully checked classifier annotation applications.

use super::*;

type AnnotationOccurrences = Vec<(u32, crate::fir::DeclarationAnnotationOccurrence)>;

struct AnnotationOwnerScope<'a> {
    class: &'a ClassDecl,
    occurrences: &'a [(u32, crate::fir::DeclarationAnnotationOccurrence)],
}

fn push_bound_suppressions(
    checker: &mut Checker<'_>,
    class: &ClassDecl,
    occurrences: &[(u32, crate::fir::DeclarationAnnotationOccurrence)],
) {
    for ((_, occurrence), arguments) in occurrences.iter().zip(&class.annotation_args) {
        if occurrence.identity_for_check() == Some(type_name("kotlin/Suppress")) {
            checker.active_statement_suppressions.extend(
                arguments
                    .iter()
                    .filter_map(|argument| checker.file.const_string_value(*argument))
                    .map(|value| value.to_lossy()),
            );
        }
    }
}

fn check_bound_classifier_annotations(
    checker: &mut Checker<'_>,
    owners: &[AnnotationOwnerScope<'_>],
    scope: &CheckerScope<'_>,
    class: &ClassDecl,
    occurrences: &[(u32, crate::fir::DeclarationAnnotationOccurrence)],
    suppresses_optional_declaration_usage: bool,
) {
    let suppression_depth = checker.active_statement_suppressions.len();
    if suppresses_optional_declaration_usage {
        checker
            .active_statement_suppressions
            .push("OPTIONAL_DECLARATION_USAGE_IN_NON_COMMON_SOURCE".to_string());
    }
    for owner in owners {
        push_bound_suppressions(checker, owner.class, owner.occurrences);
    }
    push_bound_suppressions(checker, class, occurrences);
    for ((_, occurrence), (annotation, arguments)) in occurrences
        .iter()
        .zip(class.annotations.iter().zip(&class.annotation_args))
    {
        match *occurrence {
            crate::fir::DeclarationAnnotationOccurrence::Resolved(identity)
            | crate::fir::DeclarationAnnotationOccurrence::TargetExcludedOptional(identity) => {
                checker.check_bound_annotation_application(scope, annotation, arguments, identity);
            }
            crate::fir::DeclarationAnnotationOccurrence::Unresolved => checker.diags.error(
                annotation.span,
                format!("unresolved reference '{}'.", annotation.name),
            ),
        }
    }
    checker
        .active_statement_suppressions
        .truncate(suppression_depth);
}

impl Checker<'_> {
    /// Project Pass 1's authoritative classifier annotation applications onto this source view.
    /// Retained inspection and bounded body passes still key their sidecars by parser spans, but
    /// they must not resolve or check the declaration application a second time.
    pub(super) fn install_published_classifier_annotations(
        &mut self,
        parser: DeclId,
        class: &ClassDecl,
    ) -> bool {
        let Some((stable, applications)) = self
            .active_declarations
            .zip(self.resolved_index)
            .and_then(|(active, index)| {
                let stable = active.canonical_classifier_declaration(parser, index)?;
                let applications = index
                    .checked_declaration_annotation_occurrences(stable)?
                    .to_vec();
                Some((stable, applications))
            })
        else {
            return false;
        };
        if applications.len() != class.annotations.len() {
            self.diags.error(
                class.span,
                format!(
                    "internal error: checked classifier annotation occurrences do not match \
                     stable declaration {stable:?}"
                ),
            );
            return true;
        }
        for (annotation, checked) in class.annotations.iter().zip(applications) {
            let key = (annotation.span.lo, annotation.span.hi);
            self.bound_annotation_identities
                .insert(key, checked.identity);
            if checked.target_excluded {
                self.target_excluded_annotation_occurrences.insert(key);
            }
            if let Some(application) = checked.application {
                self.applied_annotations.insert(key, application);
            }
        }
        true
    }

    pub(super) fn check_unpublished_classifier_annotations(
        &mut self,
        scope: &CheckerScope<'_>,
        class: &ClassDecl,
        published: bool,
    ) {
        if published {
            return;
        }
        for (annotation, arguments) in class.annotations.iter().zip(&class.annotation_args) {
            self.check_annotation_application(scope, annotation, arguments);
        }
    }
}

/// Fold classifier annotations against the finalized declaration index while their explicitly
/// retained argument expressions are live. Annotation classifier selection happened once during
/// header resolution; this pass carries that identity by stable declaration and source ordinal and
/// never retries source-spelling lookup.
pub(crate) fn publish_checked_classifier_annotations(
    files: &[File],
    index: &mut crate::fir::ResolvedModuleIndex,
    table: &SymbolTable,
    diags: &mut DiagSink,
) {
    for (file_index, file) in files.iter().enumerate() {
        let source = crate::fir::SourceFileId::from_raw(
            u32::try_from(file_index).expect("too many source files"),
        );
        let annotation_declarations = index.declaration_annotation_occurrence_declarations(source);
        if annotation_declarations.is_empty() {
            continue;
        }
        let active = match crate::fir::ActiveSourceDeclarations::bind_declaration_metadata(
            file,
            source,
            index,
            &annotation_declarations,
        ) {
            Ok(active) => active,
            Err(error) => {
                diags.set_file(source.raw());
                diags.error(
                    Span::new(0, 0),
                    format!(
                        "internal error: classifier annotation source inventory did not bind: \
                         {error:?}"
                    ),
                );
                return;
            }
        };
        let declarations = annotation_declarations
            .into_iter()
            .map(|stable| {
                let Some((_, class)) = active.class(file, stable) else {
                    return Err(Span::new(0, 0));
                };
                if index.classifier_header(stable).is_none() {
                    return Err(Span::new(0, 0));
                }
                let occurrences = index
                    .take_declaration_annotation_occurrences(stable)
                    .iter()
                    .enumerate()
                    .map(|(ordinal, identity)| {
                        (
                            u32::try_from(ordinal).expect("too many declaration annotations"),
                            *identity,
                        )
                    })
                    .collect::<AnnotationOccurrences>();
                let published = occurrences
                    .iter()
                    .filter_map(|(_, occurrence)| occurrence.published_identity())
                    .collect::<Vec<_>>();
                if occurrences.len() != class.annotations.len()
                    || class.annotations.len() != class.annotation_args.len()
                    || published.as_slice() != index.declaration_annotations(stable)
                {
                    return Err(class.span);
                }
                Ok((stable, class, occurrences))
            })
            .collect::<Vec<_>>();

        if let Some(span) = declarations
            .iter()
            .find_map(|declaration| match declaration {
                Err(span) => Some(*span),
                Ok(_) => None,
            })
        {
            diags.set_file(source.raw());
            diags.error(
                span,
                "internal error: stable classifier annotation occurrences do not match the \
                 finalized declaration header"
                    .to_string(),
            );
            return;
        }
        let declarations = declarations
            .into_iter()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        if declarations.is_empty() {
            continue;
        }

        diags.set_file(source.raw());
        let mut checker = make_checker_with_index(
            file,
            source.raw(),
            None,
            table,
            Some(&*index),
            Some(&active),
            None,
            diags,
        );
        let scope = CheckerScope::root();
        let mut checked = Vec::with_capacity(declarations.len());
        for (stable, class, occurrences) in &declarations {
            let mut owners = Vec::new();
            let declaration_owner = |declaration| {
                index
                    .declaration_header(declaration)
                    .and_then(|header| header.owner)
            };
            let mut owner = declaration_owner(*stable);
            let mut scope_complete = true;
            while let Some(declaration) = owner {
                if index.declaration_header(declaration).is_none() {
                    scope_complete = false;
                    break;
                }
                if index.classifier_identity(declaration).is_none() {
                    scope_complete = false;
                    break;
                }
                let Some((_, owner_class)) = active.class(file, declaration) else {
                    scope_complete = false;
                    break;
                };
                let owner_occurrences = declarations
                    .iter()
                    .find(|(candidate, _, _)| *candidate == declaration)
                    .map(|(_, _, occurrences)| occurrences.as_slice())
                    .unwrap_or_default();
                owners.push(AnnotationOwnerScope {
                    class: owner_class,
                    occurrences: owner_occurrences,
                });
                owner = declaration_owner(declaration);
            }
            if !scope_complete {
                checked.push((
                    *stable,
                    occurrences
                        .iter()
                        .map(|(_, occurrence)| {
                            crate::fir::CheckedDeclarationAnnotationOccurrence {
                                identity: occurrence.published_identity(),
                                application: None,
                                target_excluded: matches!(
                                    occurrence,
                                    crate::fir::DeclarationAnnotationOccurrence::TargetExcludedOptional(_)
                                ),
                            }
                        })
                        .collect(),
                    Vec::new(),
                    false,
                ));
                if !checker.diags.has_errors() {
                    checker.diags.error(
                        class.span,
                        "internal error: classifier annotation owner scope did not bind",
                    );
                }
                continue;
            }
            owners.reverse();
            let mut lexical_owners = Vec::new();
            let mut lexical_owner = index
                .classifier_identity(*stable)
                .and_then(TypeName::nested_owner);
            while let Some(classifier) = lexical_owner {
                lexical_owners.push(classifier);
                lexical_owner = classifier.nested_owner();
            }
            let previous_lexical_context =
                std::mem::replace(&mut checker.lexical_class_context, lexical_owners);
            check_bound_classifier_annotations(
                &mut checker,
                &owners,
                &scope,
                class,
                occurrences,
                index.declaration_suppresses_optional_declaration_usage(*stable),
            );
            checker.lexical_class_context = previous_lexical_context;

            let applications = occurrences
                .iter()
                .map(|(ordinal, occurrence)| {
                    let (identity, application, target_excluded) = match occurrence {
                        crate::fir::DeclarationAnnotationOccurrence::Resolved(identity) => {
                            let annotation = &class.annotations[*ordinal as usize];
                            let application = checker
                                .applied_annotations
                                .get(&(annotation.span.lo, annotation.span.hi))
                                .cloned();
                            (Some(*identity), application, false)
                        }
                        crate::fir::DeclarationAnnotationOccurrence::TargetExcludedOptional(_) => {
                            (None, None, true)
                        }
                        crate::fir::DeclarationAnnotationOccurrence::Unresolved => {
                            (None, None, false)
                        }
                    };
                    crate::fir::CheckedDeclarationAnnotationOccurrence {
                        identity,
                        application,
                        target_excluded,
                    }
                })
                .collect::<Vec<_>>();
            let expected = occurrences
                .iter()
                .filter(|(_, occurrence)| {
                    matches!(
                        occurrence,
                        crate::fir::DeclarationAnnotationOccurrence::Resolved(_)
                    )
                })
                .count();
            let complete = applications
                .iter()
                .zip(occurrences)
                .filter(|(checked, (_, occurrence))| {
                    checked.application.is_some()
                        && matches!(
                            occurrence,
                            crate::fir::DeclarationAnnotationOccurrence::Resolved(_)
                        )
                })
                .count()
                == expected;
            let resolved = applications
                .iter()
                .zip(occurrences)
                .filter_map(|(checked, (_, occurrence))| {
                    matches!(
                        occurrence,
                        crate::fir::DeclarationAnnotationOccurrence::Resolved(_)
                    )
                    .then(|| checked.application.as_ref())
                    .flatten()
                    .map(|application| crate::types::ResolvedAnnotation {
                        annotation: application.internal,
                        arguments: application.values.clone(),
                    })
                })
                .collect::<Vec<_>>();
            checked.push((*stable, applications, resolved, complete));
            if !complete && !checker.diags.has_errors() {
                checker.diags.error(
                    class.span,
                    "internal error: checked classifier annotations were not fully published",
                );
            }
        }
        drop(checker);
        for (declaration, applications, resolved, complete) in checked {
            if complete {
                index.publish_declaration_applied_annotations(declaration, resolved);
            }
            index.publish_checked_declaration_annotation_occurrences(declaration, applications);
        }
    }
    if index.has_declaration_annotation_occurrences() {
        diags.set_file(0);
        diags.error(
            Span::new(0, 0),
            "internal error: classifier annotation occurrences remained after stable publication",
        );
    }
}
