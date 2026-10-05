//! Type-use annotation occurrences retained by the header inventory for `@Metadata`.
//!
//! The parser files a type's annotations under the type's start offset. Pass 1 binds each
//! annotation reference to its classifier by span; this arena keeps those spans, so the
//! declared-type spellings can be built after the source AST is released. The names written on
//! function-type parameters travel the same way: `@Metadata` records each as a `@ParameterName`
//! annotation on the parameter's type.

use super::SourceFileId;
use crate::ast::File;
use crate::diag::Span;

/// One annotated type occurrence: its start offset and its annotations in source order.
type AnnotatedOccurrence = (u32, Box<[Span]>);

#[derive(Default)]
pub(super) struct HeaderTypeUseAnnotationArena {
    occurrences: std::collections::HashMap<SourceFileId, Vec<AnnotatedOccurrence>>,
    /// Per source, each named function-type parameter's type start offset and name.
    parameter_names: std::collections::HashMap<SourceFileId, Vec<(u32, Box<str>)>>,
}

impl HeaderTypeUseAnnotationArena {
    pub(super) fn add_file(&mut self, source: SourceFileId, file: &File) {
        let occurrences = file
            .type_annotations
            .iter()
            .map(|(&occurrence, annotations)| {
                let annotations = annotations
                    .iter()
                    .map(|annotation| annotation.span)
                    .collect();
                (occurrence, annotations)
            })
            .collect::<Vec<_>>();
        if !occurrences.is_empty() {
            self.occurrences.insert(source, occurrences);
        }
        let parameter_names = file
            .function_type_parameter_names
            .iter()
            .map(|(&occurrence, name)| (occurrence, name.as_str().into()))
            .collect::<Vec<_>>();
        if !parameter_names.is_empty() {
            self.parameter_names.insert(source, parameter_names);
        }
    }

    /// Each annotated type occurrence of `source`, by start offset, with its annotations in source
    /// order.
    pub(super) fn occurrences(
        &self,
        source: SourceFileId,
    ) -> impl Iterator<Item = (u32, &[Span])> + '_ {
        self.occurrences
            .get(&source)
            .into_iter()
            .flatten()
            .map(|(occurrence, annotations)| (*occurrence, &annotations[..]))
    }

    /// Each named function-type parameter of `source`: its type's start offset and its name.
    pub(super) fn parameter_names(
        &self,
        source: SourceFileId,
    ) -> impl Iterator<Item = (u32, &str)> + '_ {
        self.parameter_names
            .get(&source)
            .into_iter()
            .flatten()
            .map(|(occurrence, name)| (*occurrence, &**name))
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        let names = self
            .parameter_names
            .values()
            .flatten()
            .map(|(_, name)| std::mem::size_of::<(u32, Box<str>)>() + name.len())
            .sum::<usize>();
        names
            + self
                .occurrences
                .values()
                .flatten()
                .map(|(_, annotations)| {
                    std::mem::size_of::<AnnotatedOccurrence>()
                        + annotations.len() * std::mem::size_of::<Span>()
                })
                .sum::<usize>()
    }
}
