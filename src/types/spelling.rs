//! Type-name identities from their JVM or source spelling: a package-qualified classifier and its
//! nested segments, split at `$` or `.`.

use super::{type_name_nested_child, type_names, TypeName};
use crate::name_tree::{NameId, NameTree};

pub fn type_name(internal: &str) -> TypeName {
    if let Some(id) = type_names().get(internal) {
        return TypeName(id);
    }
    let Some((base, nested)) = split_nested_name(internal) else {
        return TypeName(type_names().insert(internal));
    };
    let mut name = TypeName(type_names().insert(base));
    for segment in nested_segments(nested) {
        name = type_name_nested_child(name, segment);
    }
    name
}
pub fn type_name_from(names: &NameTree, id: NameId) -> TypeName {
    let Some((base, nested)) = split_nested_name(names.segment(id)) else {
        return TypeName(type_names().insert_from(names, id));
    };
    let parent = names.parent(id).map_or(NameTree::ROOT, |parent| {
        type_names().insert_from(names, parent)
    });
    let mut name = TypeName(type_names().child_of(parent, base));
    for segment in nested_segments(nested) {
        name = type_name_nested_child(name, segment);
    }
    name
}

/// The public multifile facade for a part stored in `names`.
///
/// `CollectionsKt__CollectionsKt` collapses to `CollectionsKt`. The part's parent path is copied
/// into the global name tree; the `__` suffix is never rendered.
pub fn type_name_from_multifile_facade(names: &NameTree, part_id: NameId) -> TypeName {
    let segment = names.segment(part_id);
    let Some((facade_segment, _)) = segment.split_once("__") else {
        return type_name_from(names, part_id);
    };
    if facade_segment.is_empty() {
        return type_name_from(names, part_id);
    }
    let parent = names.parent(part_id).map_or(NameTree::ROOT, |parent| {
        type_names().insert_from(names, parent)
    });
    let Some((base, nested)) = split_nested_name(facade_segment) else {
        return TypeName(type_names().child_of(parent, facade_segment));
    };
    let mut name = TypeName(type_names().child_of(parent, base));
    for segment in nested_segments(nested) {
        name = type_name_nested_child(name, segment);
    }
    name
}
/// The nested classifier segments of `nested`, split at `.` and `$`. A `$` right after a separator
/// begins the next segment rather than separating an empty one, so kotlinc's `$$inlined$` copy
/// (`A$f$$inlined$g$1`) keeps its own identity instead of collapsing onto `A$f$inlined$g$1`.
fn nested_segments(nested: &str) -> impl Iterator<Item = &str> {
    let mut start = 0;
    let mut segments = Vec::new();
    for (at, byte) in nested.bytes().enumerate() {
        if !matches!(byte, b'.' | b'$') {
            continue;
        }
        if at > start {
            segments.push(&nested[start..at]);
            start = at + 1;
        } else if byte == b'.' {
            start = at + 1;
        }
    }
    if start < nested.len() {
        segments.push(&nested[start..]);
    }
    segments.into_iter()
}
fn split_nested_name(internal: &str) -> Option<(&str, &str)> {
    let classifier_start = internal.rfind('/').map_or(0, |slash| slash + 1);
    let classifier = &internal[classifier_start..];
    let separator = classifier
        .as_bytes()
        .iter()
        .position(|byte| matches!(byte, b'.' | b'$'))?;
    let base_end = classifier_start + separator;
    Some((&internal[..base_end], &internal[base_end + 1..]))
}
pub fn existing_type_name(internal: &str) -> Option<TypeName> {
    if let Some(id) = type_names().get(internal) {
        return Some(TypeName(id));
    }
    let (base, nested) = split_nested_name(internal)?;
    let mut name = TypeName(type_names().get(base)?);
    for segment in nested_segments(nested) {
        name = TypeName(type_names().existing_nested_child_of(name.name_id(), segment)?);
    }
    Some(name)
}
pub fn existing_type_name_child(parent: TypeName, segment: &str) -> Option<TypeName> {
    // Generated local/anonymous classifier names are exact declaration identities and may contain
    // `$` without having separately interned every synthetic owner prefix. Prefer that already-
    // interned child before interpreting a source-qualified/nested spelling segment by segment.
    // This performs no interning: a miss still falls through to the structural lookup below.
    if let Some(exact) = type_names().existing_child_of(parent.name_id(), segment) {
        return Some(TypeName(exact));
    }
    let Some((base, nested)) = split_nested_name(segment) else {
        return None;
    };
    let mut name = TypeName(type_names().existing_child_of(parent.name_id(), base)?);
    for segment in nested_segments(nested) {
        name = TypeName(type_names().existing_nested_child_of(name.name_id(), segment)?);
    }
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name_tree::NameTree;

    #[test]
    fn a_multifile_part_collapses_to_the_public_facade_identity() {
        let names = NameTree::default();
        let part = names.insert("kotlin/collections/CollectionsKt__CollectionsKt");
        assert_eq!(
            type_name_from_multifile_facade(&names, part),
            type_name("kotlin/collections/CollectionsKt")
        );
        let single = names.insert("kotlin/collections/MapsKt");
        assert_eq!(
            type_name_from_multifile_facade(&names, single),
            type_name("kotlin/collections/MapsKt")
        );
        let nested = names.insert("pkg/Outer$Inner__Part");
        assert_eq!(
            type_name_from_multifile_facade(&names, nested),
            type_name("pkg/Outer$Inner")
        );
    }

    #[test]
    fn a_doubled_dollar_names_its_own_class() {
        let copy = type_name("sample/AKt$f$$inlined$g$1");
        let child = type_name("sample/AKt$f$inlined$g$1");
        assert_ne!(copy, child);
        assert_eq!(copy.render(), "sample/AKt$f$$inlined$g$1");
        assert_eq!(existing_type_name("sample/AKt$f$$inlined$g$1"), Some(copy));
    }
}
