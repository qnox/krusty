//! Provider-normalized classifier facts consumed after semantic checking.

use super::*;

pub(super) fn generated_serializer_singleton(
    libraries: &JvmLibraries,
    classifier: TypeName,
) -> Option<TypeName> {
    let owner = libraries.cp.find_name(classifier)?;
    let owner_name = owner.this_class();
    let generated_serializer = type_name("kotlinx/serialization/internal/GeneratedSerializer");
    let mut candidates = owner
        .inner_classes
        .iter()
        .filter(|nested| nested.outer.as_deref() == Some(owner_name.as_str()))
        .filter_map(|nested| libraries.cp.find(&nested.inner))
        .filter(|nested| {
            nested.meta.class_kind == Some(crate::libraries::TypeKind::Object)
                && nested.interfaces.contains_name(generated_serializer)
        });
    let singleton = candidates.next()?.this_class;
    candidates.next().is_none().then_some(singleton)
}

impl crate::types::ClassifierFactSource for JvmLibraries {
    fn classifier_annotations(
        &self,
        classifier: TypeName,
    ) -> Option<Vec<crate::types::ResolvedAnnotation>> {
        SymbolSource::classifier(self, classifier).map(|shape| shape.annotations.clone())
    }

    fn classifier_is_object(&self, classifier: TypeName) -> Option<bool> {
        SymbolSource::classifier(self, classifier)
            .map(|shape| shape.kind == crate::libraries::TypeKind::Object)
    }

    fn classifier_declaration(
        &self,
        classifier: TypeName,
    ) -> Option<crate::types::ClassifierDeclarationFacts> {
        let shape = SymbolSource::classifier(self, classifier)?;
        Some(crate::types::ClassifierDeclarationFacts {
            kind: shape.kind.into(),
            is_abstract: shape.inheritance.is_abstract || !shape.sealed_subclasses.is_empty(),
            own_type_parameter_count: shape.own_type_parameter_count,
            companion: shape
                .companion_object
                .as_ref()
                .map(|(field, companion)| (Box::from(field.as_str()), *companion)),
            qualified_name: shape.qualified_name.clone(),
            source: shape.source_file.is_some(),
        })
    }

    fn generated_serializer_singleton(&self, classifier: TypeName) -> Option<TypeName> {
        SymbolSource::generated_serializer_singleton(self, classifier)
    }

    fn has_serialization_serializer_accessor(
        &self,
        companion: TypeName,
        type_parameters: usize,
    ) -> bool {
        SymbolSource::has_serialization_serializer_accessor(self, companion, type_parameters)
    }

    fn serialization_companion(&self, classifier: TypeName) -> Option<(Box<str>, TypeName)> {
        let shape = SymbolSource::classifier(self, classifier)?;
        shape
            .companion_object
            .as_ref()
            .map(|(field, companion)| (Box::from(field.as_str()), *companion))
    }

    fn classifier_value_underlying(&self, classifier: TypeName) -> Option<Ty> {
        SymbolSource::classifier(self, classifier).and_then(|shape| shape.value_underlying)
    }

    fn classifier_value_property(&self, classifier: TypeName) -> Option<String> {
        SymbolSource::classifier(self, classifier)
            .and_then(|shape| shape.value_underlying_property.clone())
    }
}
