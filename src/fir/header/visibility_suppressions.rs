//! Compact `@Suppress` argument facts retained by the header inventory for signature solving.

use super::{DeclarationId, DeclarationKind, DeclarationStub, SourceFileId};
use crate::ast::{Decl, File};
use crate::diag::Span;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HeaderVisibilitySuppressionApplication {
    pub annotation: Span,
    pub invisible_reference: bool,
    pub invisible_member: bool,
    pub optional_declaration_usage: bool,
}

/// Compact `@Suppress` argument facts needed while signatures are solved after ordinary expression
/// arenas have been released. Annotation identity is deliberately not guessed here: the ordinary
/// resolver later joins `annotation` to its resolved classifier and accepts these flags only for
/// `kotlin.Suppress`.
#[derive(Default)]
pub(super) struct HeaderVisibilitySuppressionArena {
    files: std::collections::HashMap<SourceFileId, Vec<HeaderVisibilitySuppressionApplication>>,
    declarations:
        std::collections::HashMap<DeclarationId, Vec<HeaderVisibilitySuppressionApplication>>,
}

impl HeaderVisibilitySuppressionArena {
    fn applications(
        file: &File,
        annotations: &[crate::ast::AnnotationRef],
        arguments: &[Vec<crate::ast::ExprId>],
    ) -> Vec<HeaderVisibilitySuppressionApplication> {
        annotations
            .iter()
            .zip(arguments)
            .filter_map(|(annotation, arguments)| {
                let mut invisible_reference = false;
                let mut invisible_member = false;
                let mut optional_declaration_usage = false;
                for argument in arguments {
                    let Some(value) = file.const_string_value(*argument) else {
                        continue;
                    };
                    match value.as_str() {
                        Some("INVISIBLE_REFERENCE") => invisible_reference = true,
                        Some("INVISIBLE_MEMBER") => invisible_member = true,
                        Some("OPTIONAL_DECLARATION_USAGE_IN_NON_COMMON_SOURCE") => {
                            optional_declaration_usage = true
                        }
                        _ => {}
                    }
                }
                (invisible_reference || invisible_member || optional_declaration_usage).then_some(
                    HeaderVisibilitySuppressionApplication {
                        annotation: annotation.span,
                        invisible_reference,
                        invisible_member,
                        optional_declaration_usage,
                    },
                )
            })
            .collect()
    }

    pub(super) fn add_file(
        &mut self,
        source: SourceFileId,
        file: &File,
        stubs: &[DeclarationStub],
    ) {
        let file_applications = file
            .file_annotations
            .iter()
            .filter_map(|(annotation, arguments)| {
                Self::applications(
                    file,
                    std::slice::from_ref(annotation),
                    std::slice::from_ref(arguments),
                )
                .into_iter()
                .next()
            })
            .collect::<Vec<_>>();
        if !file_applications.is_empty() {
            self.files.insert(source, file_applications);
        }

        let mut record = |range: Span,
                          kind: DeclarationKind,
                          annotations: &[crate::ast::AnnotationRef],
                          arguments: &[Vec<crate::ast::ExprId>]| {
            let applications = Self::applications(file, annotations, arguments);
            if let (false, Some(declaration)) = (
                applications.is_empty(),
                stubs
                    .iter()
                    .find(|stub| stub.range == range && stub.kind == kind)
                    .map(|stub| stub.id),
            ) {
                self.declarations
                    .entry(declaration)
                    .or_default()
                    .extend(applications);
            }
        };
        for &declaration in &file.decls {
            match file.decl(declaration) {
                Decl::Fun(function) => record(
                    function.span,
                    DeclarationKind::Function,
                    &function.annotations,
                    &function.annotation_args,
                ),
                Decl::Property(property) => record(
                    property.span,
                    DeclarationKind::Property,
                    &property.annotations,
                    &property.annotation_args,
                ),
                Decl::Class(class) => {
                    record(
                        class.span,
                        DeclarationKind::Classifier,
                        &class.annotations,
                        &class.annotation_args,
                    );
                    if let Some(annotations) = &class.primary_ctor_annotations {
                        record(
                            class.span,
                            DeclarationKind::Constructor,
                            annotations,
                            &class.primary_ctor_annotation_args,
                        );
                    }
                    for function in &class.methods {
                        record(
                            function.span,
                            DeclarationKind::Function,
                            &function.annotations,
                            &function.annotation_args,
                        );
                    }
                    for property in &class.body_props {
                        record(
                            property.span,
                            DeclarationKind::Property,
                            &property.annotations,
                            &property.annotation_args,
                        );
                    }
                    for entry in &class.enum_entries {
                        for function in &entry.methods {
                            record(
                                function.span,
                                DeclarationKind::Function,
                                &function.annotations,
                                &function.annotation_args,
                            );
                        }
                        for property in &entry.props {
                            record(
                                property.span,
                                DeclarationKind::Property,
                                &property.annotations,
                                &property.annotation_args,
                            );
                        }
                    }
                }
            }
        }
    }

    pub(super) fn file(&self, source: SourceFileId) -> &[HeaderVisibilitySuppressionApplication] {
        self.files
            .get(&source)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(super) fn declaration(
        &self,
        declaration: DeclarationId,
    ) -> &[HeaderVisibilitySuppressionApplication] {
        self.declarations
            .get(&declaration)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.files
            .values()
            .chain(self.declarations.values())
            .map(|applications| {
                applications.len() * std::mem::size_of::<HeaderVisibilitySuppressionApplication>()
            })
            .sum()
    }
}
