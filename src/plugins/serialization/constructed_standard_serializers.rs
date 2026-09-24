//! Serialization-plugin ABI mappings for standard Kotlin types whose serializers are constructed
//! over the serializers of their checked type arguments.

use crate::types::{type_name, TypeName};

/// The kotlinx serializer class kotlinc's plugin selects for a standard Kotlin classifier.
///
/// This table owns only the stable classifier-to-serializer ABI mapping. The selected semantic
/// type supplies the argument serializers; this mapping must not infer their count from a name.
pub(super) fn constructed_standard_serializer(classifier: TypeName) -> Option<TypeName> {
    if [
        "kotlin/collections/List",
        "kotlin/collections/MutableList",
        "kotlin/collections/Collection",
        "kotlin/collections/MutableCollection",
        "kotlin/collections/Iterable",
        "kotlin/collections/MutableIterable",
    ]
    .into_iter()
    .map(type_name)
    .any(|candidate| candidate == classifier)
    {
        Some(type_name(
            "kotlinx/serialization/internal/ArrayListSerializer",
        ))
    } else if ["kotlin/collections/Set", "kotlin/collections/MutableSet"]
        .into_iter()
        .map(type_name)
        .any(|candidate| candidate == classifier)
    {
        Some(type_name(
            "kotlinx/serialization/internal/LinkedHashSetSerializer",
        ))
    } else if ["kotlin/collections/Map", "kotlin/collections/MutableMap"]
        .into_iter()
        .map(type_name)
        .any(|candidate| candidate == classifier)
    {
        Some(type_name(
            "kotlinx/serialization/internal/LinkedHashMapSerializer",
        ))
    } else if classifier == type_name("kotlin/Pair") {
        Some(type_name("kotlinx/serialization/internal/PairSerializer"))
    } else if classifier == type_name("kotlin/Triple") {
        Some(type_name("kotlinx/serialization/internal/TripleSerializer"))
    } else if classifier == type_name("kotlin/collections/Map.Entry") {
        Some(type_name(
            "kotlinx/serialization/internal/MapEntrySerializer",
        ))
    } else {
        None
    }
}
