//! Callable references to Java bean properties derived from a getter.
//!
//! kotlinc's tower puts such a synthetic property below the member functions of its name, so
//! `Thread::isDaemon` names the function `isDaemon()`; a declared property ranks ahead of them as
//! before. The synthetic property is selected only when no member function is applicable (an
//! expected `KProperty1<Thread, Boolean>`), and without `ReferencesToSyntheticJavaProperties` that
//! selection is then reported as unsupported.

use super::*;
use crate::symbol_resolver::ResolvedPropertyRef;

/// Split a selected property reference into its rung: a declared property, which ranks ahead of the
/// member functions, or a synthetic one, which ranks after them.
pub(super) fn by_level(
    property: Option<ResolvedPropertyRef>,
) -> (Option<ResolvedPropertyRef>, Option<ResolvedPropertyRef>) {
    match property {
        Some(property) if property.java_synthetic => (None, Some(property)),
        declared => (declared, None),
    }
}

impl Checker<'_> {
    /// Report a selected synthetic-property reference the language does not support.
    pub(super) fn rejects_synthetic_property_reference(
        &mut self,
        expression: ExprId,
        property: &ResolvedPropertyRef,
    ) -> bool {
        if !property.java_synthetic || self.file.language.references_to_synthetic_java_properties {
            return false;
        }
        self.diags.error(
            self.member_name_span(expression, &property.name),
            "the feature \"references to synthetic java properties\" is unsupported.".to_string(),
        );
        true
    }
}
