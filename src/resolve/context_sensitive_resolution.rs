//! Classifier selection contributed by an expression or type position's expected type.
//!
//! This module owns only the additional classifier-scope rung. Value/property selection stays in
//! the ordinary checker so it records the same semantic targets as explicitly qualified source.

use crate::symbol_resolver::InheritedNestedClassifier;
use crate::symbol_resolver::{direct_supertypes, inherited_nested_classifier_name};
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName};

pub(super) fn expected_nested_classifier(
    source: &dyn SymbolSource,
    expected: Ty,
    name: &str,
) -> InheritedNestedClassifier {
    let Some(root) = expected.non_null().obj_internal() else {
        return InheritedNestedClassifier::NotFound;
    };
    inherited_nested_classifier_name(
        name,
        vec![root],
        |owner| {
            direct_supertypes(source, Ty::obj_name(owner))
                .into_iter()
                .filter_map(Ty::obj_internal)
                .collect()
        },
        |candidate: TypeName| source.classifier(candidate).is_some(),
    )
}
