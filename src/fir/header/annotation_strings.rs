//! Constant string arguments of declaration annotations, extracted once while streaming headers.

use super::{DeclarationId, File};

/// Provider-neutral constant payload for declaration annotations needed during signature solving.
/// The arena is deliberately generic: `@JvmName`, plugin annotations, and future signature-affecting
/// annotations must not each invent a private copy of Pass-1 expression extraction.
#[derive(Default)]
pub(super) struct HeaderAnnotationStringArena {
    /// Entries are parallel to the declaration's annotation list. Empty argument slices preserve
    /// ordinals without retaining annotation spellings or source ranges.
    declarations: std::collections::HashMap<DeclarationId, Vec<Box<[Box<str>]>>>,
}

impl HeaderAnnotationStringArena {
    pub(super) fn add(
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
