//! Stable publication of fully checked classifier annotation applications.

use super::*;

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
        let active =
            match crate::fir::ActiveSourceDeclarations::bind_complete_source(file, source, index) {
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
        let declarations = file
            .decl_arena
            .iter()
            .enumerate()
            .filter_map(|(raw, declaration)| {
                let Decl::Class(class) = declaration else {
                    return None;
                };
                let parser = DeclId(
                    u32::try_from(raw).expect("source declaration arena exceeds packed identity"),
                );
                if file.is_local_declaration(parser) {
                    return None;
                }
                let stable = active.canonical_classifier_declaration(parser, index)?;
                index.classifier_header(stable)?;
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
                    .collect::<Vec<_>>();
                let published = occurrences
                    .iter()
                    .filter_map(|(_, identity)| *identity)
                    .collect::<Vec<_>>();
                if occurrences.len() != class.annotations.len()
                    || class.annotations.len() != class.annotation_args.len()
                    || published.as_slice() != index.declaration_annotations(stable)
                {
                    return Some(Err(class.span));
                }
                (!published.is_empty()).then_some(Ok((stable, class, occurrences)))
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
        for (stable, class, occurrences) in declarations {
            let suppression_depth = checker.active_statement_suppressions.len();
            for ((_, identity), arguments) in occurrences.iter().zip(&class.annotation_args) {
                if *identity == Some(type_name("kotlin/Suppress")) {
                    checker.active_statement_suppressions.extend(
                        arguments
                            .iter()
                            .filter_map(|argument| file.const_string_value(*argument))
                            .map(|value| value.to_lossy()),
                    );
                }
            }
            for ((_, identity), (annotation, arguments)) in occurrences
                .iter()
                .zip(class.annotations.iter().zip(&class.annotation_args))
            {
                let Some(identity) = *identity else {
                    continue;
                };
                checker.check_bound_annotation_application(&scope, annotation, arguments, identity);
            }
            checker
                .active_statement_suppressions
                .truncate(suppression_depth);

            let applications = occurrences
                .iter()
                .filter_map(|(ordinal, identity)| {
                    identity.map(|_| {
                        let annotation = &class.annotations[*ordinal as usize];
                        checker
                            .applied_annotations
                            .get(&(annotation.span.lo, annotation.span.hi))
                            .map(|application| crate::types::ResolvedAnnotation {
                                annotation: application.internal,
                                arguments: application.values.clone(),
                            })
                    })?
                })
                .collect::<Vec<_>>();
            let expected = occurrences
                .iter()
                .filter(|(_, identity)| identity.is_some())
                .count();
            if applications.len() == expected {
                checked.push((stable, applications));
            } else if !checker.diags.has_errors() {
                checker.diags.error(
                    class.span,
                    "internal error: checked classifier annotations were not fully published",
                );
            }
        }
        drop(checker);
        for (declaration, applications) in checked {
            index.publish_declaration_applied_annotations(declaration, applications);
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
