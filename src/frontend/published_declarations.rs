//! The checked declaration facts a source-set analysis publishes by stable declaration identity,
//! for a consumer that reads a source set's declarations without compiling it (a KSP round).

use super::{FrontendDeclarationId, SourceSetAnalysis};
use crate::fir::{DeclarationFlags, DeclarationKind, PropertyAccessor, ResolvedModuleIndex};
use crate::types::{AnnotationValue, ResolvedAnnotation};

impl SourceSetAnalysis {
    fn published_index(&self) -> Option<&ResolvedModuleIndex> {
        Some(self.streamed.as_ref()?.module.index())
    }

    /// The checked applications written on the function, property, constructor or type alias
    /// `declaration`, in source order.
    pub fn declaration_annotations(
        &self,
        declaration: FrontendDeclarationId,
    ) -> &[ResolvedAnnotation] {
        self.published_index()
            .map(|index| index.declaration_applied_annotations(declaration))
            .unwrap_or_default()
    }

    /// The checked applications written on the value parameter at `position` of the function or
    /// constructor `callable`, in source order. A primary constructor's `val`/`var` parameter has
    /// every application written on it here, whatever declaration each lands on.
    pub fn parameter_annotations(
        &self,
        callable: FrontendDeclarationId,
        position: u32,
    ) -> &[ResolvedAnnotation] {
        self.published_index()
            .map(|index| index.parameter_applied_annotations(callable, position))
            .unwrap_or_default()
    }

    /// The checked applications on the getter, or with `setter` the setter, of `property`: those
    /// targeted at it from the property (`@get:A`) first, then those written on it.
    pub fn accessor_annotations(
        &self,
        property: FrontendDeclarationId,
        setter: bool,
    ) -> &[ResolvedAnnotation] {
        let accessor = if setter {
            PropertyAccessor::Setter
        } else {
            PropertyAccessor::Getter
        };
        self.published_index()
            .map(|index| index.property_accessor_applied_annotations(property, accessor))
            .unwrap_or_default()
    }

    /// The folded defaults of the elements the source annotation class `class` declares, in
    /// declaration order, or `None` when they were not published.
    pub fn annotation_element_defaults(
        &self,
        class: FrontendDeclarationId,
    ) -> Option<&[(String, AnnotationValue)]> {
        self.published_index()?.annotation_element_defaults(class)
    }

    /// The constructor of the classifier `owner` at `position`: 0 for the primary constructor, and
    /// one more than its index among the written secondary constructors for a secondary one.
    pub fn constructor_declaration(
        &self,
        owner: FrontendDeclarationId,
        position: u32,
    ) -> Option<FrontendDeclarationId> {
        self.published_index()?
            .owned_declaration(owner, DeclarationKind::Constructor, position)
    }

    /// The property the classifier `owner` declares at `position`: among the primary
    /// constructor's parameters with `constructor_parameter`, else among its body properties.
    pub fn member_property_declaration(
        &self,
        owner: FrontendDeclarationId,
        position: u32,
        constructor_parameter: bool,
    ) -> Option<FrontendDeclarationId> {
        let index = self.published_index()?;
        index
            .owned_declarations(owner)
            .iter()
            .copied()
            .find(|declaration| {
                index
                    .declaration_anchor(*declaration)
                    .is_some_and(|anchor| {
                        anchor.kind == DeclarationKind::Property && anchor.sibling == position
                    })
                    && index
                        .declaration_header(*declaration)
                        .is_some_and(|header| {
                            header.flags.has(DeclarationFlags::PROPERTY_PARAMETER)
                                == constructor_parameter
                        })
            })
    }

    /// The top-level property of the source at `source` (its position in the analysis) that is
    /// the declaration at `position` among the file's top-level declarations.
    pub fn top_level_property_declaration(
        &self,
        source: u32,
        position: u32,
    ) -> Option<FrontendDeclarationId> {
        let index = self.published_index()?;
        index
            .source_inventory(crate::fir::SourceFileId::from_raw(source))
            .iter()
            .copied()
            .find(|declaration| {
                index
                    .declaration_anchor(*declaration)
                    .is_some_and(|anchor| {
                        anchor.owner.is_none()
                            && anchor.kind == DeclarationKind::Property
                            && anchor.sibling == position
                    })
            })
    }
}
