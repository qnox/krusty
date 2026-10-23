//! Declaration records the serialization frontend plugin publishes for later phases.
//!
//! Each record is the plugin's own decision, made while Pass 1's checked declarations are
//! authoritative; a later phase reads it by declaration identity and never re-derives it from
//! annotations, generated spellings or storage.

use super::*;

#[derive(Debug, Default, PartialEq)]
pub(super) struct SerializationRecords {
    /// Return-value statuses of the exact runtime declarations overridden by serialization-plugin
    /// generated members. Pass 1 resolves this closed contract from full semantic shapes.
    generated_member_statuses: Option<crate::types::SerializationGeneratedMemberStatuses>,
    /// Exact companion accessor contributed by the serialization frontend plugin for a source
    /// classifier. The callable was selected while the generated symbol table was authoritative;
    /// later consumers receive its owner and arity without recognizing a generated spelling.
    companion_accessors: HashMap<DeclarationId, (Box<str>, TypeName, usize)>,
    /// Primary constructor of the custom serializer CLASS a source classifier's
    /// `@Serializable(with = …)` names, selected and validated by the serialization frontend, with
    /// the generated accessor's operand ordinal passed to each constructor parameter.
    custom_serializer_constructors: HashMap<DeclarationId, (DeclarationId, Box<[u32]>)>,
    /// Constructor plans selected for serializer classes named on declared type occurrences, keyed
    /// by serializer identity and the annotated type's semantic argument count. This is independent
    /// of source property spelling and can be reused by every equal checked occurrence.
    type_use_serializer_constructors:
        HashMap<(TypeName, u32), ResolvedTypeUseSerializerConstruction>,
    /// The serial elements of each source class whose `write$Self` and deserialization
    /// constructor the plugin commits to generating, for a subclass in another file to inherit.
    serializable_class_elements: HashMap<DeclarationId, crate::types::SerializableClassElements>,
}

impl SerializationRecords {
    pub(super) fn is_empty(&self) -> bool {
        self.generated_member_statuses.is_none()
            && self.companion_accessors.is_empty()
            && self.custom_serializer_constructors.is_empty()
            && self.type_use_serializer_constructors.is_empty()
            && self.serializable_class_elements.is_empty()
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.companion_accessors.len()
            * (std::mem::size_of::<DeclarationId>()
                + std::mem::size_of::<(Box<str>, TypeName, usize)>())
            + self
                .companion_accessors
                .values()
                .map(|(field, _, _)| field.len())
                .sum::<usize>()
            + self.custom_serializer_constructors.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<(DeclarationId, Box<[u32]>)>())
            + self
                .custom_serializer_constructors
                .values()
                .map(|(_, operands)| std::mem::size_of_val(operands.as_ref()))
                .sum::<usize>()
            + self.type_use_serializer_constructors.len()
                * (std::mem::size_of::<(TypeName, u32)>()
                    + std::mem::size_of::<ResolvedTypeUseSerializerConstruction>())
            + self
                .type_use_serializer_constructors
                .values()
                .map(|construction| {
                    std::mem::size_of_val(construction.parameters.as_ref())
                        + std::mem::size_of_val(construction.operands.as_ref())
                })
                .sum::<usize>()
            + self.serializable_class_elements.len()
                * (std::mem::size_of::<DeclarationId>()
                    + std::mem::size_of::<crate::types::SerializableClassElements>())
            + self
                .serializable_class_elements
                .values()
                .map(|record| {
                    std::mem::size_of_val(record.elements.as_ref())
                        + record
                            .elements
                            .iter()
                            .map(|element| element.name.len())
                            .sum::<usize>()
                })
                .sum::<usize>()
    }
}

impl ResolvedModuleIndex {
    pub fn serialization_generated_member_statuses(
        &self,
    ) -> Option<crate::types::SerializationGeneratedMemberStatuses> {
        self.serialization.generated_member_statuses
    }

    pub(crate) fn publish_serialization_generated_member_statuses(
        &mut self,
        statuses: crate::types::SerializationGeneratedMemberStatuses,
    ) {
        assert!(
            self.serialization
                .generated_member_statuses
                .replace(statuses)
                .is_none(),
            "serialization generated-member statuses may be published only once"
        );
    }

    pub fn serialization_companion_accessor(
        &self,
        declaration: DeclarationId,
    ) -> Option<(&str, TypeName, usize)> {
        self.serialization
            .companion_accessors
            .get(&declaration)
            .map(|(field, companion, arity)| (field.as_ref(), *companion, *arity))
    }

    /// The custom serializer class's primary constructor selected for `declaration`'s
    /// `@Serializable(with = …)`, with the accessor operand ordinal for each of its parameters.
    pub fn serialization_custom_serializer_constructor(
        &self,
        declaration: DeclarationId,
    ) -> Option<(DeclarationId, &[u32])> {
        self.serialization
            .custom_serializer_constructors
            .get(&declaration)
            .map(|(constructor, operands)| (*constructor, operands.as_ref()))
    }

    pub fn serialization_type_use_serializer_constructors(
        &self,
    ) -> impl Iterator<Item = (&(TypeName, u32), &ResolvedTypeUseSerializerConstruction)> {
        self.serialization.type_use_serializer_constructors.iter()
    }

    /// The serial elements the serialization plugin committed to for source class `declaration`;
    /// `None` when it generates no `write$Self` for the class or cannot describe every element.
    pub fn serializable_class_elements(
        &self,
        declaration: DeclarationId,
    ) -> Option<&crate::types::SerializableClassElements> {
        self.serialization
            .serializable_class_elements
            .get(&declaration)
    }

    pub fn publish_serialization_companion_accessor(
        &mut self,
        declaration: DeclarationId,
        field: Box<str>,
        companion: TypeName,
        type_parameters: usize,
    ) {
        assert!(
            self.serialization
                .companion_accessors
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
            self.serialization
                .custom_serializer_constructors
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
            .serialization
            .type_use_serializer_constructors
            .get(&key)
        {
            assert_eq!(
                existing, &construction,
                "equal checked serializer occurrences must select one constructor declaration"
            );
            return;
        }
        self.serialization
            .type_use_serializer_constructors
            .insert(key, construction);
    }

    pub(crate) fn publish_serializable_class_elements(
        &mut self,
        declaration: DeclarationId,
        elements: crate::types::SerializableClassElements,
    ) {
        assert!(
            self.serialization
                .serializable_class_elements
                .insert(declaration, elements)
                .is_none(),
            "a source class may publish its serial elements only once"
        );
    }
}
