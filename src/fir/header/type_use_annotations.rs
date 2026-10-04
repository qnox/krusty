//! Type-use annotation occurrences retained by the header inventory for `@Metadata`.
//!
//! The parser files a type's annotations under the type's start offset. Pass 1 binds each
//! annotation reference to its classifier by span; this arena keeps the span and whether the
//! application had arguments, so the declared-type spellings can be built after the source AST is
//! released.

use super::SourceFileId;
use crate::ast::File;
use crate::diag::Span;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HeaderTypeUseAnnotation {
    pub annotation: Span,
    pub has_arguments: bool,
}

/// One annotated type occurrence: its start offset and its annotations in source order.
type AnnotatedOccurrence = (u32, Box<[HeaderTypeUseAnnotation]>);

#[derive(Default)]
pub(super) struct HeaderTypeUseAnnotationArena {
    occurrences: std::collections::HashMap<SourceFileId, Vec<AnnotatedOccurrence>>,
}

impl HeaderTypeUseAnnotationArena {
    pub(super) fn add_file(&mut self, source: SourceFileId, file: &File) {
        let occurrences = file
            .type_annotations
            .iter()
            .map(|(&occurrence, annotations)| {
                let annotations = annotations
                    .iter()
                    .map(|annotation| HeaderTypeUseAnnotation {
                        annotation: annotation.span,
                        has_arguments: file
                            .type_annotation_arguments
                            .contains_key(&(annotation.span.lo, annotation.span.hi)),
                    })
                    .collect();
                (occurrence, annotations)
            })
            .collect::<Vec<_>>();
        if !occurrences.is_empty() {
            self.occurrences.insert(source, occurrences);
        }
    }

    /// Each annotated type occurrence of `source`, by start offset, with its annotations in source
    /// order.
    pub(super) fn occurrences(
        &self,
        source: SourceFileId,
    ) -> impl Iterator<Item = (u32, &[HeaderTypeUseAnnotation])> + '_ {
        self.occurrences
            .get(&source)
            .into_iter()
            .flatten()
            .map(|(occurrence, annotations)| (*occurrence, &annotations[..]))
    }

    pub(super) fn storage_payload_bytes(&self) -> usize {
        self.occurrences
            .values()
            .flatten()
            .map(|(_, annotations)| {
                std::mem::size_of::<AnnotatedOccurrence>()
                    + annotations.len() * std::mem::size_of::<HeaderTypeUseAnnotation>()
            })
            .sum()
    }
}
