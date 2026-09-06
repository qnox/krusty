//! Language-defined factory lookup for collection-literal syntax.
//!
//! A collection literal first offers the expected classifier's companion `operator fun of`.
//! Kotlin additionally defines qualified standard-library factories for its built-in collection
//! interfaces. This module owns only that syntax-to-symbol convention; candidate applicability,
//! overload selection, generic inference, and call commitment remain in the ordinary resolver.

use crate::types::{type_name, TypeName};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StandardCollectionFactory {
    pub(super) package: TypeName,
    pub(super) callable: &'static str,
}

/// Return the qualified standard-library factory associated with a semantic classifier identity.
///
/// Arrays are deliberately absent: the checker handles their compiler-synthetic construction
/// before consulting this table. Matching is by interned qualified identity, never source spelling.
pub(super) fn standard_factory(classifier: TypeName) -> Option<StandardCollectionFactory> {
    let (package, callable) = if classifier.matches("kotlin/collections/List") {
        ("kotlin/collections", "listOf")
    } else if classifier.matches("kotlin/collections/MutableList") {
        ("kotlin/collections", "mutableListOf")
    } else if classifier.matches("kotlin/collections/Set") {
        ("kotlin/collections", "setOf")
    } else if classifier.matches("kotlin/collections/MutableSet") {
        ("kotlin/collections", "mutableSetOf")
    } else if classifier.matches("kotlin/sequences/Sequence") {
        ("kotlin/sequences", "sequenceOf")
    } else {
        return None;
    };
    Some(StandardCollectionFactory {
        package: type_name(package),
        callable,
    })
}

/// With no expected classifier Kotlin's collection-literal fallback is an immutable `List`.
pub(super) fn default_factory() -> StandardCollectionFactory {
    StandardCollectionFactory {
        package: type_name("kotlin/collections"),
        callable: "listOf",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_factories_are_selected_by_qualified_classifier_identity() {
        assert_eq!(
            standard_factory(type_name("kotlin/collections/List")),
            Some(StandardCollectionFactory {
                package: type_name("kotlin/collections"),
                callable: "listOf",
            })
        );
        assert_eq!(standard_factory(type_name("sample/List")), None);
    }
}
