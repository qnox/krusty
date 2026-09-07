//! Compact `@Suppress` facts retained after the active parser unit is released.

use crate::ast::{Decl, File};
use crate::diag::Span;

use super::{DeclarationId, DeclarationIds, DeclarationKind, DeclarationStub, SourceFileId};

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
        ids: &DeclarationIds,
        primary_declarations: &[Option<DeclarationId>],
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

        let children = stubs
            .iter()
            .filter_map(|stub| {
                let anchor = ids.anchor(stub.id)?;
                Some(((anchor.owner?, anchor.kind, anchor.sibling), stub.id))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut record = |declaration: Option<DeclarationId>,
                          annotations: &[crate::ast::AnnotationRef],
                          arguments: &[Vec<crate::ast::ExprId>]| {
            let applications = Self::applications(file, annotations, arguments);
            if let (false, Some(declaration)) = (applications.is_empty(), declaration) {
                self.declarations
                    .entry(declaration)
                    .or_default()
                    .extend(applications);
            }
        };
        for &declaration in &file.decls {
            let stable = primary_declarations
                .get(declaration.0 as usize)
                .copied()
                .flatten();
            match file.decl(declaration) {
                Decl::Fun(function) => {
                    record(stable, &function.annotations, &function.annotation_args)
                }
                Decl::Property(property) => {
                    record(stable, &property.annotations, &property.annotation_args)
                }
                Decl::Class(class) => {
                    record(stable, &class.annotations, &class.annotation_args);
                    if let (Some(owner), Some(annotations)) =
                        (stable, &class.primary_ctor_annotations)
                    {
                        record(
                            children
                                .get(&(owner, DeclarationKind::Constructor, 0))
                                .copied(),
                            annotations,
                            &class.primary_ctor_annotation_args,
                        );
                    }
                    let Some(owner) = stable else {
                        continue;
                    };
                    for (index, function) in class.methods.iter().enumerate() {
                        record(
                            children
                                .get(&(
                                    owner,
                                    DeclarationKind::Function,
                                    u32::try_from(index).expect("too many class methods"),
                                ))
                                .copied(),
                            &function.annotations,
                            &function.annotation_args,
                        );
                    }
                    for (index, property) in class.body_props.iter().enumerate() {
                        record(
                            children
                                .get(&(
                                    owner,
                                    DeclarationKind::Property,
                                    u32::try_from(index).expect("too many class properties"),
                                ))
                                .copied(),
                            &property.annotations,
                            &property.annotation_args,
                        );
                    }
                    for (entry_index, entry_declaration) in class.enum_entries.iter().enumerate() {
                        let stable_entry = children
                            .get(&(
                                owner,
                                DeclarationKind::EnumEntry,
                                u32::try_from(entry_index).expect("too many enum entries"),
                            ))
                            .copied();
                        for (index, function) in entry_declaration.methods.iter().enumerate() {
                            record(
                                stable_entry.and_then(|owner| {
                                    children
                                        .get(&(
                                            owner,
                                            DeclarationKind::Function,
                                            u32::try_from(index)
                                                .expect("too many enum-entry methods"),
                                        ))
                                        .copied()
                                }),
                                &function.annotations,
                                &function.annotation_args,
                            );
                        }
                        for (index, property) in entry_declaration.props.iter().enumerate() {
                            record(
                                stable_entry.and_then(|owner| {
                                    children
                                        .get(&(
                                            owner,
                                            DeclarationKind::Property,
                                            u32::try_from(index)
                                                .expect("too many enum-entry properties"),
                                        ))
                                        .copied()
                                }),
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
