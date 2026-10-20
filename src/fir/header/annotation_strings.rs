//! Constant string arguments of declaration annotations, extracted once while streaming headers.

use super::{DeclarationAnchor, DeclarationId, DeclarationIds, DeclarationKind, File};
use crate::ast::Decl;

/// Provider-neutral constant payload for declaration annotations needed during signature solving.
/// The arena is deliberately generic: `@JvmName`, plugin annotations, and future signature-affecting
/// annotations must not each invent a private copy of Pass-1 expression extraction.
#[derive(Default)]
pub(super) struct HeaderAnnotationStringArena {
    /// Entries are parallel to the declaration's annotation list. Empty argument slices preserve
    /// ordinals without retaining annotation spellings or source ranges.
    declarations: crate::fx_hash::HashMap<DeclarationId, Vec<Box<[Box<str>]>>>,
}

impl HeaderAnnotationStringArena {
    /// Record the annotation arguments of every top-level declaration of `file` and of the
    /// functions and properties its classes declare. A member's arguments are read by other files
    /// exactly like a top-level declaration's, under the stable identity the inventory interned.
    pub(super) fn add_file(
        &mut self,
        file: &File,
        source: crate::fir::SourceFileId,
        stable_by_transient: &crate::fx_hash::HashMap<crate::ast::DeclId, DeclarationId>,
        declarations: &DeclarationIds,
    ) {
        for &declaration in &file.decls {
            let Some(&stable) = stable_by_transient.get(&declaration) else {
                continue;
            };
            match file.decl(declaration) {
                Decl::Fun(function) => {
                    self.add(
                        stable,
                        file,
                        &function.annotations,
                        &function.annotation_args,
                    );
                }
                Decl::Property(property) => {
                    self.add(
                        stable,
                        file,
                        &property.annotations,
                        &property.annotation_args,
                    );
                }
                Decl::Class(class) => {
                    self.add(stable, file, &class.annotations, &class.annotation_args);
                    let member = |range, kind, sibling: usize| {
                        declarations.get(DeclarationAnchor {
                            source,
                            range,
                            owner: Some(stable),
                            kind,
                            sibling: u32::try_from(sibling).expect("too many class members"),
                        })
                    };
                    for (sibling, method) in class.methods.iter().enumerate() {
                        if let Some(member) =
                            member(method.span, DeclarationKind::Function, sibling)
                        {
                            self.add(member, file, &method.annotations, &method.annotation_args);
                        }
                    }
                    for (sibling, property) in class.body_props.iter().enumerate() {
                        if let Some(member) =
                            member(property.span, DeclarationKind::Property, sibling)
                        {
                            self.add(
                                member,
                                file,
                                &property.annotations,
                                &property.annotation_args,
                            );
                        }
                    }
                }
            }
        }
    }

    fn add(
        &mut self,
        declaration: DeclarationId,
        file: &File,
        annotations: &[crate::ast::AnnotationRef],
        source_arguments: &[Vec<crate::ast::ExprId>],
    ) {
        let arguments = annotations
            .iter()
            .zip(source_arguments)
            .map(|(_, arguments)| {
                arguments
                    .iter()
                    .filter_map(|argument| file.const_string_value(*argument))
                    .filter_map(|value| value.as_str().map(|value| value.into()))
                    .collect::<Vec<Box<str>>>()
                    .into_boxed_slice()
            })
            .collect::<Vec<_>>();
        if arguments.iter().any(|arguments| !arguments.is_empty()) {
            self.declarations.insert(declaration, arguments);
        }
    }

    pub(super) fn arguments(
        &self,
        declaration: DeclarationId,
        annotation_ordinal: usize,
    ) -> &[Box<str>] {
        self.declarations
            .get(&declaration)
            .and_then(|annotations| annotations.get(annotation_ordinal))
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.declarations
            .values()
            .map(|annotations| {
                annotations.len() * std::mem::size_of::<Box<[Box<str>]>>()
                    + annotations
                        .iter()
                        .flatten()
                        .map(|argument| argument.len())
                        .sum::<usize>()
                    + annotations
                        .iter()
                        .map(|arguments| arguments.len() * std::mem::size_of::<Box<str>>())
                        .sum::<usize>()
            })
            .sum()
    }
}
