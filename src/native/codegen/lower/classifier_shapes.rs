//! Target representation shapes read from frozen classifier facts.
//!
//! The provider publishes semantic roles and direct supertypes before backend lowering starts.
//! This module turns those facts into the native runtime's finite set of representation markers;
//! it never derives a role from a classifier spelling.

use crate::backend::BackendClassifierSource;
use crate::types::{ClassifierRole, CollectionKind, MappedCollection, TypeName};

use super::super::super::intrinsics::{self, CollectionShape};

/// The runtime collection shape reached by this classifier's declared supertype graph.
///
/// Looking through supertypes is necessary for concrete implementations, ranges, and primitive
/// iterators: providers publish the mapping on the collection interface declaration rather than
/// copying it onto every subtype.
pub(super) fn collection_shape(
    classifiers: &dyn BackendClassifierSource,
    internal: TypeName,
) -> Option<CollectionShape> {
    if intrinsics::is_sequence_type(internal) {
        return Some(CollectionShape::Sequence);
    }
    if internal == crate::types::wk::string()
        || intrinsics::is_char_sequence(internal)
        || intrinsics::is_string_builder(internal)
    {
        return Some(CollectionShape::Text);
    }

    let mut pending = vec![internal];
    let mut visited = std::collections::HashSet::new();
    let collection = loop {
        let classifier = pending.pop()?;
        if !visited.insert(classifier) {
            continue;
        }
        let Some(fact) = classifiers.classifier(classifier) else {
            continue;
        };
        if let Some(ClassifierRole::MappedCollection(collection)) = fact.role {
            break collection;
        }
        pending.extend(fact.supertypes.iter().copied());
    };
    Some(match collection.kind {
        CollectionKind::Iterator | CollectionKind::ListIterator => CollectionShape::Iterator,
        CollectionKind::Map => CollectionShape::Map,
        CollectionKind::MapEntry => CollectionShape::MapEntry,
        CollectionKind::Iterable
        | CollectionKind::Collection
        | CollectionKind::List
        | CollectionKind::Set => CollectionShape::Iterable,
    })
}

/// Whether a type check names the read-only List face represented by the runtime's List marker.
pub(super) fn is_list_check_type(
    classifiers: &dyn BackendClassifierSource,
    internal: TypeName,
) -> bool {
    matches!(
        classifiers.classifier(internal).and_then(|fact| fact.role),
        Some(ClassifierRole::MappedCollection(MappedCollection {
            kind: CollectionKind::List,
            mutable: false,
        }))
    )
}

/// The native runtime marker for a published non-suspend function classifier.
pub(super) fn function_type_descriptor(
    classifiers: &dyn BackendClassifierSource,
    internal: TypeName,
) -> Option<&'static str> {
    const ARITIES: [&str; 23] = [
        "kt_type_function0",
        "kt_type_function1",
        "kt_type_function2",
        "kt_type_function3",
        "kt_type_function4",
        "kt_type_function5",
        "kt_type_function6",
        "kt_type_function7",
        "kt_type_function8",
        "kt_type_function9",
        "kt_type_function10",
        "kt_type_function11",
        "kt_type_function12",
        "kt_type_function13",
        "kt_type_function14",
        "kt_type_function15",
        "kt_type_function16",
        "kt_type_function17",
        "kt_type_function18",
        "kt_type_function19",
        "kt_type_function20",
        "kt_type_function21",
        "kt_type_function22",
    ];
    if internal == crate::types::wk::function_root() {
        return Some("kt_type_function");
    }
    let ClassifierRole::FunctionOfArity(arity) = classifiers.classifier(internal)?.role? else {
        return None;
    };
    ARITIES.get(usize::from(arity)).copied()
}
