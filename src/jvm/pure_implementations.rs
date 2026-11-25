//! The platform's pure-implementation map: Java collection classes that kotlinc treats as pure
//! implementations of a Kotlin mutable collection interface
//! (`FakePureImplementationsProvider`). A pure implementation states its type arguments rigidly
//! through that edge, so `ArrayList<String>.add(null)` is rejected although a Java declaration's
//! own `E!` would accept it. The map is identical at every language version kotlinc supports
//! (`ConcurrentMapPurelyImplemented` only concerns `ConcurrentHashMap`, which is listed here too).

use crate::types::{CollectionKind, TypeName};

const PURE_IMPLEMENTATIONS: [(&str, CollectionKind); 10] = [
    ("java/util/ArrayList", CollectionKind::List),
    ("java/util/LinkedList", CollectionKind::List),
    ("java/util/HashSet", CollectionKind::Set),
    ("java/util/LinkedHashSet", CollectionKind::Set),
    ("java/util/TreeSet", CollectionKind::Set),
    ("java/util/HashMap", CollectionKind::Map),
    ("java/util/LinkedHashMap", CollectionKind::Map),
    ("java/util/TreeMap", CollectionKind::Map),
    (
        "java/util/concurrent/ConcurrentHashMap",
        CollectionKind::Map,
    ),
    (
        "java/util/concurrent/ConcurrentSkipListMap",
        CollectionKind::Map,
    ),
];

/// The Kotlin mutable collection the JVM class `internal` purely implements, if it does.
pub(super) fn purely_implemented_supertype(internal: TypeName) -> Option<CollectionKind> {
    PURE_IMPLEMENTATIONS
        .iter()
        .find(|(class, _)| internal.matches(class))
        .map(|(_, kind)| *kind)
}
