//! What a source's file annotations publish for every later phase, resolved once in Pass 1 while
//! their syntax is still live: the lexical policies they open (`@file:Suppress`, `@file:OptIn`) and
//! their checked applications, whose folded values a backend reads (`@file:JvmName("Facade")`).

use super::{ResolvedModuleIndex, SourceFileId};
use crate::lexical_policy::LexicalPolicy;
use crate::types::ResolvedAnnotation;

/// One source's published file-annotation facts.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct FileAnnotationFacts {
    pub(crate) policies: Box<[LexicalPolicy]>,
    pub(crate) applications: Box<[ResolvedAnnotation]>,
}

impl FileAnnotationFacts {
    pub(super) fn heap_size(&self) -> usize {
        std::mem::size_of::<SourceFileId>()
            + std::mem::size_of_val(self.policies.as_ref())
            + std::mem::size_of_val(self.applications.as_ref())
    }
}

impl ResolvedModuleIndex {
    pub(crate) fn publish_file_annotations(
        &mut self,
        source: SourceFileId,
        facts: FileAnnotationFacts,
    ) {
        let previous = self.file_annotations.insert(source, facts);
        assert!(
            previous.is_none(),
            "a source's file annotations are published once"
        );
    }

    pub(crate) fn file_lexical_policies(&self, source: SourceFileId) -> &[LexicalPolicy] {
        self.file_annotations
            .get(&source)
            .map_or(&[], |facts| &facts.policies[..])
    }

    /// Every source's checked file-annotation applications, for sources that declare any.
    pub(crate) fn file_annotation_applications(
        &self,
    ) -> impl Iterator<Item = (SourceFileId, &[ResolvedAnnotation])> {
        self.file_annotations
            .iter()
            .map(|(&source, facts)| (source, &facts.applications[..]))
    }
}
