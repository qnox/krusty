//! Semantic roles a classifier declaration can play in type checks and casts.
//!
//! Providers publish these on the classifier records they normalize: the collection mapping comes
//! from the platform's builtin class map, and a function classifier's arity from its declared
//! callable signature. Consumers read the published role; they never recover it from a name.

/// Which Kotlin collection interface a platform builtin represents. A platform's authenticated
/// builtin map pairs its read-only and mutable faces; on JVM this is kotlinc's
/// `JavaToKotlinClassMap`, while a KLIB provider validates its selected stdlib's interface headers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CollectionKind {
    Iterator,
    Iterable,
    Collection,
    List,
    ListIterator,
    Set,
    Map,
    MapEntry,
}

/// A Kotlin collection classifier mapped onto a platform collection interface, and which of its two
/// faces it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct MappedCollection {
    pub kind: CollectionKind,
    pub mutable: bool,
}

impl MappedCollection {
    /// Whether Kotlin gives this member a SPECIAL BRIDGE, so that a call through the wide type does
    /// not always reach an override at all.
    ///
    /// `Map<Any, Any>.get(key: Any)` is declared with a NON-NULL parameter, and a caller holding
    /// the same object as a `Map<Any?, Any?>` may pass `null`. Kotlin does not call the override
    /// there: it answers the member's default (`null` for `get` and `remove`, `false` for a
    /// `contains`, `-1` for an `indexOf`) because the argument cannot be what the declaration
    /// accepts. The corpus asks exactly that (`specialBuiltins/notEmptyMap.kt`,
    /// `bridges/special.kt`). Every one of them takes an argument, which is why the nullary
    /// members are unaffected.
    pub fn has_special_bridge(self, name: &str) -> bool {
        let over_a_map = matches!(self.kind, CollectionKind::Map | CollectionKind::MapEntry);
        let over_a_collection = matches!(
            self.kind,
            CollectionKind::Iterable
                | CollectionKind::Collection
                | CollectionKind::List
                | CollectionKind::Set
        );
        match name {
            "get" | "containsKey" | "containsValue" | "getOrDefault" => over_a_map,
            // `MutableCollection.remove(element)` is as special as `Map.remove(key)`.
            "remove" => over_a_map || over_a_collection,
            "contains" | "indexOf" | "lastIndexOf" => over_a_collection,
            _ => false,
        }
    }
}

/// What a classifier declaration is, for the checks a plain instance test or cast cannot decide.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ClassifierRole {
    /// One face of a mapped collection classifier.
    MappedCollection(MappedCollection),
    /// A non-suspend, non-reflective `FunctionN` classifier of this arity.
    FunctionOfArity(u8),
    /// A non-reflective `SuspendFunctionN` classifier of this source arity.
    SuspendFunctionOfArity(u8),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_roles_keep_their_special_bridges() {
        let collection = |kind, mutable| MappedCollection { kind, mutable };
        assert!(collection(CollectionKind::Collection, false).has_special_bridge("contains"));
        assert!(collection(CollectionKind::Collection, true).has_special_bridge("remove"));
        assert!(collection(CollectionKind::Map, false).has_special_bridge("remove"));
        assert!(collection(CollectionKind::Map, false).has_special_bridge("get"));
        assert!(!collection(CollectionKind::Collection, false).has_special_bridge("get"));
        assert!(!collection(CollectionKind::Map, false).has_special_bridge("size"));
    }
}
