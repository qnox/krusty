//! Whole-index state queries, kept separate from declaration publication and lookup.

use super::*;

impl ResolvedModuleIndex {
    pub fn len(&self) -> usize {
        self.signatures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
            && self.declaration_headers.is_empty()
            && self.declaration_annotations.is_empty()
            && self.declaration_annotation_occurrences.is_empty()
            && self.declaration_applied_annotations.is_empty()
            && self.declaration_annotation_string_arguments.is_empty()
            && self.declaration_annotation_class_arguments.is_empty()
            && self.continuation_ordinals.is_empty()
            && self.local_class_name_provenance.is_empty()
            && self.generated_classifiers.is_empty()
            && self.serialization_companion_accessors.is_empty()
            && self.classifiers.is_empty()
            && self.signatures.is_empty()
            && self.callables.is_empty()
            && self.callable_default_providers.is_empty()
            && self.callable_equality_bounds.is_empty()
            && self.callable_inherited_statuses.is_empty()
            && self.property_return_value_statuses.is_empty()
            && self.function_typed_parameter_callables.is_empty()
            && self.properties.is_empty()
    }
}
