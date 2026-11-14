//! Language-defined factory lookup for collection-literal syntax.
//!
//! A collection literal first offers the expected classifier's companion `operator fun of`.
//! Kotlin additionally defines qualified standard-library factories for its built-in collection
//! interfaces. This module owns only that syntax-to-symbol convention; candidate applicability,
//! overload selection, generic inference, and call commitment remain in the ordinary resolver.

use crate::ast::ExprId;
use crate::libraries::FunctionInfo;
use crate::symbol_resolver::CallArgKind;
use crate::types::{type_name, Ty, TypeName};

use super::Checker;

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

impl Checker<'_> {
    /// Whether a selected parameter can supply a language-defined construction strategy for a
    /// collection literal. This is intentionally a target-availability query, not a second call
    /// resolver: overload applicability for the factory itself is checked exactly once when the
    /// enclosing callable has won and `check_collection_literal` commits the expression.
    pub(super) fn collection_literal_target_available(&self, expected: Ty) -> bool {
        let expected = expected.non_null();
        expected.array_elem().is_some()
            || expected.kotlin_class_internal().is_some_and(|classifier| {
                self.classifier_constructs_collection_literals(classifier)
            })
    }

    fn classifier_constructs_collection_literals(&self, classifier: TypeName) -> bool {
        if standard_factory(classifier).is_some() {
            return true;
        }
        self.resolver()
            .classifier_call_candidates(classifier, "of")
            .into_iter()
            .flat_map(|(_, candidates)| candidates)
            .chain(
                self.resolver()
                    .classifier_associated_callables(classifier, "of"),
            )
            .any(|candidate| candidate.flags.operator)
    }

    /// Argument kinds for selecting among `candidates` with each collection literal postponed.
    ///
    /// An expectation-free literal reads as `List<T>`, which is not evidence against a candidate
    /// whose parameter builds the literal itself. The kind instead carries the classifiers among
    /// the candidates' parameters that construct literals, so selection admits those parameters
    /// and the selected parameter then becomes the literal's expected type.
    pub(super) fn postponed_collection_literal_kinds(
        &self,
        args: &[ExprId],
        kinds: Vec<CallArgKind>,
        candidates: &[FunctionInfo],
    ) -> Vec<CallArgKind> {
        if !args
            .iter()
            .any(|arg| self.file.collection_literal_calls.contains(&arg.0))
        {
            return kinds;
        }
        let mut factory_classifiers = Vec::new();
        for parameter in candidates
            .iter()
            .flat_map(|candidate| candidate.semantic_signature().params.clone())
        {
            if let Some(classifier) = parameter.non_null().kotlin_class_internal() {
                if !factory_classifiers.contains(&classifier)
                    && self.classifier_constructs_collection_literals(classifier)
                {
                    factory_classifiers.push(classifier);
                }
            }
        }
        let factory_classifiers: std::sync::Arc<[TypeName]> = factory_classifiers.into();
        args.iter()
            .zip(kinds)
            .map(|(arg, kind)| {
                if self.file.collection_literal_calls.contains(&arg.0) {
                    CallArgKind::CollectionLiteral {
                        provisional: kind.ty(),
                        factory_classifiers: factory_classifiers.clone(),
                    }
                } else {
                    kind
                }
            })
            .collect()
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
