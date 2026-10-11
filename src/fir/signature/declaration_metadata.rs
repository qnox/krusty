//! Stable declaration metadata publication and readback.

use super::*;

/// The superclass constructor a generated no-arg constructor (kotlinc's no-arg plugin) delegates
/// to, selected by the frontend.
///
/// A selected constructor with parameters has a default for every one of them; the generated
/// constructor calls it with all of them omitted, as kotlinc does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoArgSuperConstructor {
    /// A source constructor of the superclass, by declaration.
    Declared(DeclarationId),
    /// A dependency superclass's constructor, with its parameter types.
    External {
        declaration: ExternalCallableId,
        parameters: Vec<ResolvedTy>,
    },
    /// A constructor any subclass may call as `<init>()`: `Any`'s, or the no-arg constructor the
    /// plugin generates for a matched superclass.
    Unrestricted,
}

impl ResolvedModuleIndex {
    /// The position `declaration`'s continuation class takes in its scope's generated-class
    /// sequence, or `None` when the declaration is not a source `suspend` function.
    pub fn continuation_ordinal(&self, declaration: DeclarationId) -> Option<u32> {
        self.continuation_ordinals.get(&declaration).copied()
    }

    pub fn publish_continuation_ordinal(&mut self, declaration: DeclarationId, ordinal: u32) {
        assert!(
            self.continuation_ordinals
                .insert(declaration, ordinal)
                .is_none_or(|existing| existing == ordinal),
            "a suspend declaration holds exactly one continuation ordinal"
        );
    }

    /// The superclass constructor `classifier`'s generated no-arg constructor delegates to, or
    /// `None` when the no-arg plugin gives it none.
    pub fn no_arg_constructor(&self, classifier: DeclarationId) -> Option<&NoArgSuperConstructor> {
        self.no_arg_constructors.get(&classifier)
    }

    pub fn publish_no_arg_constructor(
        &mut self,
        classifier: DeclarationId,
        superclass_constructor: NoArgSuperConstructor,
    ) {
        assert!(
            self.no_arg_constructors
                .insert(classifier, superclass_constructor)
                .is_none(),
            "a classifier holds at most one generated no-arg constructor"
        );
    }

    /// Every published type alias's expansion spelling, for the one stable-metadata step that
    /// seals its type-use annotations (see [`crate::spelling::TypeUseAnnotation`]).
    pub(crate) fn type_alias_spellings_mut(
        &mut self,
    ) -> impl Iterator<Item = &mut crate::spelling::Spelled> + '_ {
        self.type_aliases
            .values_mut()
            .map(|header| &mut header.expansion_spelling)
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

    /// The custom serializer class's primary constructor selected for `declaration`'s
    /// `@Serializable(with = …)`, with the accessor operand ordinal for each of its parameters.
    pub fn serialization_custom_serializer_constructor(
        &self,
        declaration: DeclarationId,
    ) -> Option<(DeclarationId, &[u32])> {
        self.serialization_custom_serializer_constructors
            .get(&declaration)
            .map(|(constructor, operands)| (*constructor, operands.as_ref()))
    }

    pub fn serialization_type_use_serializer_constructors(
        &self,
    ) -> impl Iterator<Item = (&(TypeName, u32), &ResolvedTypeUseSerializerConstruction)> {
        self.serialization_type_use_serializer_constructors.iter()
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

    pub fn publish_serialization_custom_serializer_constructor(
        &mut self,
        declaration: DeclarationId,
        constructor: DeclarationId,
        operands: Box<[u32]>,
    ) {
        assert!(
            self.serialization_custom_serializer_constructors
                .insert(declaration, (constructor, operands))
                .is_none(),
            "a source classifier may publish only one custom serializer constructor"
        );
    }

    pub fn publish_serialization_type_use_serializer_constructor(
        &mut self,
        type_argument_count: u32,
        construction: ResolvedTypeUseSerializerConstruction,
    ) {
        let key = (construction.serializer, type_argument_count);
        if let Some(existing) = self
            .serialization_type_use_serializer_constructors
            .get(&key)
        {
            assert_eq!(
                existing, &construction,
                "equal checked serializer occurrences must select one constructor declaration"
            );
            return;
        }
        self.serialization_type_use_serializer_constructors
            .insert(key, construction);
    }
}
