//! Stable declaration metadata publication and readback.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeclarationAnnotationOccurrence {
    Resolved(TypeName),
    TargetExcludedOptional(TypeName),
    Unresolved,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CheckedDeclarationAnnotationOccurrence {
    pub(crate) identity: Option<TypeName>,
    pub(crate) application: Option<crate::types::AppliedAnnotation>,
    pub(crate) target_excluded: bool,
}

impl DeclarationAnnotationOccurrence {
    pub(crate) fn published_identity(self) -> Option<TypeName> {
        match self {
            Self::Resolved(identity) => Some(identity),
            Self::TargetExcludedOptional(_) | Self::Unresolved => None,
        }
    }

    pub(crate) fn identity_for_check(self) -> Option<TypeName> {
        match self {
            Self::Resolved(identity) | Self::TargetExcludedOptional(identity) => Some(identity),
            Self::Unresolved => None,
        }
    }
}

impl ResolvedModuleIndex {
    pub fn declaration_annotations(&self, declaration: DeclarationId) -> &[TypeName] {
        self.declaration_annotations
            .get(&declaration)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub(crate) fn declaration_annotation_occurrence_declarations(
        &self,
        source: SourceFileId,
    ) -> Vec<DeclarationId> {
        let mut declarations = self
            .declaration_annotation_occurrences
            .keys()
            .copied()
            .filter(|declaration| {
                self.declaration_anchor(*declaration)
                    .is_some_and(|anchor| anchor.source == source)
            })
            .collect::<Vec<_>>();
        declarations.sort_by_key(|declaration| {
            (
                self.source_order(*declaration).unwrap_or(u32::MAX),
                *declaration,
            )
        });
        declarations
    }

    pub(crate) fn take_declaration_annotation_occurrences(
        &mut self,
        declaration: DeclarationId,
    ) -> Box<[DeclarationAnnotationOccurrence]> {
        self.declaration_annotation_occurrences
            .remove(&declaration)
            .unwrap_or_default()
    }

    pub(crate) fn has_declaration_annotation_occurrences(&self) -> bool {
        !self.declaration_annotation_occurrences.is_empty()
    }

    pub fn declaration_applied_annotations(
        &self,
        declaration: DeclarationId,
    ) -> &[crate::types::ResolvedAnnotation] {
        self.declaration_applied_annotations
            .get(&declaration)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub(crate) fn checked_declaration_annotation_occurrences(
        &self,
        declaration: DeclarationId,
    ) -> Option<&[CheckedDeclarationAnnotationOccurrence]> {
        self.checked_declaration_annotation_occurrences
            .get(&declaration)
            .map(Box::as_ref)
    }

    pub fn declaration_annotation_string_arguments(
        &self,
        declaration: DeclarationId,
        annotation_ordinal: u32,
    ) -> &[Box<str>] {
        self.declaration_annotation_string_arguments
            .get(&(declaration, annotation_ordinal))
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub fn declaration_annotation_class_arguments(
        &self,
        declaration: DeclarationId,
        annotation_ordinal: u32,
    ) -> &[TypeName] {
        self.declaration_annotation_class_arguments
            .get(&(declaration, annotation_ordinal))
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub fn generated_classifiers(
        &self,
        declaration: DeclarationId,
    ) -> &[crate::types::GeneratedClassifierFact] {
        self.generated_classifiers
            .get(&declaration)
            .map(Box::as_ref)
            .unwrap_or_default()
    }

    pub fn serialization_companion_accessor(
        &self,
        declaration: DeclarationId,
    ) -> Option<(&str, TypeName, usize)> {
        self.serialization_companion_accessors
            .get(&declaration)
            .map(|(field, companion, arity)| (field.as_ref(), *companion, *arity))
    }

    pub fn publish_declaration_annotations(
        &mut self,
        declaration: DeclarationId,
        annotations: impl IntoIterator<Item = TypeName>,
    ) {
        let annotations = annotations
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if annotations.is_empty() {
            return;
        }
        assert!(
            self.declaration_annotations
                .insert(declaration, annotations)
                .is_none(),
            "a stable declaration may publish its annotation identities only once"
        );
    }

    pub(crate) fn publish_declaration_annotation_occurrences(
        &mut self,
        declaration: DeclarationId,
        annotations: impl IntoIterator<Item = DeclarationAnnotationOccurrence>,
    ) {
        let annotations = annotations
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if annotations.is_empty() {
            return;
        }
        assert!(
            self.declaration_annotation_occurrences
                .insert(declaration, annotations)
                .is_none(),
            "a stable declaration may publish its annotation occurrences only once"
        );
    }

    pub fn publish_declaration_applied_annotations(
        &mut self,
        declaration: DeclarationId,
        annotations: impl IntoIterator<Item = crate::types::ResolvedAnnotation>,
    ) {
        let annotations = annotations
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if annotations.is_empty() {
            return;
        }
        assert!(
            self.declaration_applied_annotations
                .insert(declaration, annotations)
                .is_none(),
            "a stable declaration may publish checked annotation applications only once"
        );
    }

    pub(crate) fn publish_checked_declaration_annotation_occurrences(
        &mut self,
        declaration: DeclarationId,
        annotations: impl IntoIterator<Item = CheckedDeclarationAnnotationOccurrence>,
    ) {
        let annotations = annotations
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        assert!(
            !annotations.is_empty(),
            "only an annotation-bearing declaration has checked occurrences"
        );
        assert!(
            self.checked_declaration_annotation_occurrences
                .insert(declaration, annotations)
                .is_none(),
            "a stable declaration may publish its checked annotation occurrences only once"
        );
    }

    pub fn publish_declaration_annotation_string_arguments(
        &mut self,
        declaration: DeclarationId,
        annotation_ordinal: u32,
        arguments: impl IntoIterator<Item = Box<str>>,
    ) {
        let arguments = arguments.into_iter().collect::<Vec<_>>().into_boxed_slice();
        if arguments.is_empty() {
            return;
        }
        assert!(
            self.declaration_annotation_string_arguments
                .insert((declaration, annotation_ordinal), arguments)
                .is_none(),
            "a stable annotation occurrence may publish its string arguments only once"
        );
    }

    pub fn publish_declaration_annotation_class_arguments(
        &mut self,
        declaration: DeclarationId,
        annotation_ordinal: u32,
        arguments: impl IntoIterator<Item = TypeName>,
    ) {
        let arguments = arguments.into_iter().collect::<Vec<_>>().into_boxed_slice();
        if arguments.is_empty() {
            return;
        }
        assert!(
            self.declaration_annotation_class_arguments
                .insert((declaration, annotation_ordinal), arguments)
                .is_none(),
            "a stable annotation occurrence may publish its class arguments only once"
        );
    }

    pub fn publish_generated_classifiers(
        &mut self,
        declaration: DeclarationId,
        classifiers: impl IntoIterator<Item = crate::types::GeneratedClassifierFact>,
    ) {
        let classifiers = classifiers
            .into_iter()
            .collect::<Vec<_>>()
            .into_boxed_slice();
        if classifiers.is_empty() {
            return;
        }
        assert!(
            self.generated_classifiers
                .insert(declaration, classifiers)
                .is_none(),
            "a stable declaration may publish generated classifiers only once"
        );
    }

    pub fn publish_serialization_companion_accessor(
        &mut self,
        declaration: DeclarationId,
        field: Box<str>,
        companion: TypeName,
        type_parameters: usize,
    ) {
        assert!(
            self.serialization_companion_accessors
                .insert(declaration, (field, companion, type_parameters))
                .is_none(),
            "a source classifier may publish only one serialization companion accessor"
        );
    }
}
