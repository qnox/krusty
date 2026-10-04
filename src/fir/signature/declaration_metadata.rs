//! Stable declaration metadata publication and readback.

use super::*;

impl ResolvedModuleIndex {
    pub fn declaration_spellings(
        &self,
        declaration: DeclarationId,
    ) -> Option<&crate::spelling::DeclaredSpellings> {
        self.declaration_spellings.get(&declaration)
    }

    /// Publish the source spellings of a declaration's declared types. An inferred result's
    /// spelling, published by signature solving beforehand, fills the result slot the source left
    /// unspelled.
    pub(crate) fn publish_declaration_spellings(
        &mut self,
        declaration: DeclarationId,
        mut spellings: crate::spelling::DeclaredSpellings,
    ) {
        assert!(
            self.declaration_headers.contains_key(&declaration),
            "declaration spellings require a published semantic header"
        );
        if let Some(inferred) = self.declaration_spellings.remove(&declaration) {
            let only_result = crate::spelling::DeclaredSpellings {
                ret: crate::spelling::Spelled::default(),
                ..inferred.clone()
            };
            assert!(
                spellings.ret.is_none() && only_result.is_none(),
                "a declaration may publish source spellings only once"
            );
            spellings.ret = inferred.ret;
        }
        self.declaration_spellings.insert(declaration, spellings);
    }

    /// Publish the alias spelling of an inferred declaration result (`val sb = StringBuilder()`).
    pub(crate) fn publish_inferred_result_spelling(
        &mut self,
        declaration: DeclarationId,
        ret: crate::spelling::Spelled,
    ) {
        assert!(
            self.declaration_headers.contains_key(&declaration),
            "declaration spellings require a published semantic header"
        );
        let spellings = crate::spelling::DeclaredSpellings {
            ret,
            ..crate::spelling::DeclaredSpellings::default()
        };
        assert!(
            self.declaration_spellings
                .insert(declaration, spellings)
                .is_none(),
            "an inferred result publishes its spelling once, before the declared spellings"
        );
    }

    pub fn declaration_annotations(&self, declaration: DeclarationId) -> &[TypeName] {
        self.declaration_annotations
            .get(&declaration)
            .map(Box::as_ref)
            .unwrap_or_default()
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
