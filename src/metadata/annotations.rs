//! The annotations one declaration hands to `@Metadata`.

use crate::ir::{AppliedAnnotation, DeclarationAnnotations};

/// What `@Metadata` records about one declaration's annotations, for every owner alike (class,
/// constructor, function, property, value parameter): the `HAS_ANNOTATIONS` flag and the
/// annotation records written beside it.
///
/// The flag is the frontend's fact that the source declaration declares a non-SOURCE annotation.
/// It travels explicitly and is never reconstructed from the records: an `@OptionalExpectation`
/// application this platform erased leaves no record, yet kotlinc still sets the flag (it derives
/// it from FIR, before the erasure).
#[derive(Clone, Debug, Default)]
pub struct MetadataAnnotations {
    declared: bool,
    records: Vec<AppliedAnnotation>,
}

/// A declaration with no annotations, for an owner that records none.
pub static NO_ANNOTATIONS: MetadataAnnotations = MetadataAnnotations {
    declared: false,
    records: Vec::new(),
};

impl MetadataAnnotations {
    /// A declaration's checked annotations, recorded in declaration order.
    pub fn of(annotations: &DeclarationAnnotations) -> Self {
        Self::with_records(annotations, annotations.applications().cloned().collect())
    }

    /// [`Self::of`] for a declaration that may carry no annotation table at all.
    pub fn of_optional(annotations: Option<&DeclarationAnnotations>) -> Self {
        annotations.map(Self::of).unwrap_or_default()
    }

    /// A declaration's checked annotations with the records in the order the backend writes them
    /// (the class-file order of a primary constructor's attributes, for example). `records` holds
    /// exactly the retained applications of `annotations`.
    pub fn with_records(
        annotations: &DeclarationAnnotations,
        records: Vec<AppliedAnnotation>,
    ) -> Self {
        debug_assert_eq!(records.len(), annotations.applications().count());
        Self {
            declared: annotations.declares_annotations(),
            records,
        }
    }

    /// Whether the declaration's `flags` carry `HAS_ANNOTATIONS`.
    pub fn declares_annotations(&self) -> bool {
        self.declared
    }

    /// The annotation records, written only where kotlinc's `AnnotationsInMetadata` applies.
    pub fn records(&self) -> &[AppliedAnnotation] {
        &self.records
    }

    /// The annotation records, for a target renaming the classifiers they name.
    pub(crate) fn records_mut(&mut self) -> &mut [AppliedAnnotation] {
        &mut self.records
    }
}

/// What `@Metadata` records about a property's accessors' own annotations: the getter's and the
/// setter's `HAS_ANNOTATIONS` bits and records (`Property.getter_annotation` f15,
/// `setter_annotation` f16), and the setter value parameter's.
#[derive(Clone, Debug, Default)]
pub struct AccessorMetadataAnnotations {
    pub getter: MetadataAnnotations,
    pub setter: MetadataAnnotations,
    pub setter_parameter: MetadataAnnotations,
}

impl AccessorMetadataAnnotations {
    /// A property's checked accessor annotations, recorded in declaration order.
    pub fn of(annotations: &crate::ir::AccessorAnnotations) -> Self {
        Self {
            getter: MetadataAnnotations::of(&annotations.getter),
            setter: MetadataAnnotations::of(&annotations.setter),
            setter_parameter: MetadataAnnotations::of(&annotations.setter_parameter),
        }
    }
}
