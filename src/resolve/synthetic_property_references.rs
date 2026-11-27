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

    /// Whether an expected reference type admits only a property reference: its classifier reaches
    /// `KProperty` and not `KFunction`. A function reference's `KFunctionN` is then no subtype of
    /// it, so kotlinc finds no member function applicable (`val p: KProperty1<Thread, Boolean> =
    /// Thread::isDaemon` selects the synthetic property).
    pub(super) fn expects_property_reference(&self, expected: Option<Ty>) -> bool {
        let Some(expected) = expected.map(Ty::non_null).filter(|ty| !ty.is_fun()) else {
            return false;
        };
        let source = self.fed_source();
        let mut pending = std::collections::VecDeque::from([expected]);
        let mut seen = crate::fx_hash::HashSet::default();
        let mut property = false;
        while let Some(current) = pending.pop_front() {
            let Some(internal) = current.obj_internal() else {
                continue;
            };
            if !seen.insert(internal) {
                continue;
            }
            if internal.matches(crate::types::KFUNCTION_INTERNAL) {
                return false;
            }
            property |= internal.matches("kotlin/reflect/KProperty");
            pending.extend(crate::symbol_resolver::direct_supertypes(&source, current));
        }
        property
    }
}
