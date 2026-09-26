//! kotlinc's `TypeIntrinsics`: the type checks and casts a JVM `instanceof`/`checkcast` cannot
//! express.
//!
//! A mutable Kotlin collection shares its JVM interface with its read-only face, so `x is
//! MutableList<*>` asks `kotlin.jvm.internal.TypeIntrinsics.isMutableList`, which also rejects a
//! Kotlin read-only implementation. A function type erases to `FunctionN`, which every lambda of
//! any arity may implement through `FunctionBase`, so `x is (Int) -> Int` asks
//! `isFunctionOfArity(x, 1)`. Both apply to a type operation's semantic target: a written one, and
//! a reified argument once it is substituted.

use crate::ir::TypeCheckRole;
use crate::types::wk::{CollectionKind, MappedCollection};

const TYPE_INTRINSICS: &str = "kotlin/jvm/internal/TypeIntrinsics";

const MAPPED_MARKER: &str = "kotlin/jvm/internal/markers/KMappedMarker";

/// kotlinc's `KOTLIN_MARKER_INTERFACES`: the marker interface a class implementing a Kotlin
/// collection classifier also implements, which is what the `TypeIntrinsics` checks read at run
/// time. A read-only face is a `KMappedMarker`; a mutable one is its own `KMutableX`.
pub(crate) fn collection_marker(collection: MappedCollection) -> &'static str {
    if !collection.mutable {
        return MAPPED_MARKER;
    }
    match collection.kind {
        CollectionKind::Iterator => "kotlin/jvm/internal/markers/KMutableIterator",
        CollectionKind::Iterable => "kotlin/jvm/internal/markers/KMutableIterable",
        CollectionKind::Collection => "kotlin/jvm/internal/markers/KMutableCollection",
        CollectionKind::List => "kotlin/jvm/internal/markers/KMutableList",
        CollectionKind::ListIterator => "kotlin/jvm/internal/markers/KMutableListIterator",
        CollectionKind::Set => "kotlin/jvm/internal/markers/KMutableSet",
        CollectionKind::Map => "kotlin/jvm/internal/markers/KMutableMap",
        CollectionKind::MapEntry => "kotlin/jvm/internal/markers/KMutableMap$Entry",
    }
}

/// The suffix of kotlinc's `isMutableX`/`asMutableX` for a collection kind.
fn suffix(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Iterator => "Iterator",
        CollectionKind::Iterable => "Iterable",
        CollectionKind::Collection => "Collection",
        CollectionKind::List => "List",
        CollectionKind::ListIterator => "ListIterator",
        CollectionKind::Set => "Set",
        CollectionKind::Map => "Map",
        CollectionKind::MapEntry => "MapEntry",
    }
}

/// The JVM interface both faces of a collection kind erase to.
fn jvm_interface(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Iterator => "java/util/Iterator",
        CollectionKind::Iterable => "java/lang/Iterable",
        CollectionKind::Collection => "java/util/Collection",
        CollectionKind::List => "java/util/List",
        CollectionKind::ListIterator => "java/util/ListIterator",
        CollectionKind::Set => "java/util/Set",
        CollectionKind::Map => "java/util/Map",
        CollectionKind::MapEntry => "java/util/Map$Entry",
    }
}

/// One `invokestatic` on `TypeIntrinsics`, and the `int` arity pushed before it, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IntrinsicCall {
    pub(crate) arity: Option<u8>,
    pub(crate) name: String,
    pub(crate) descriptor: String,
}

impl IntrinsicCall {
    pub(crate) fn owner(&self) -> &'static str {
        TYPE_INTRINSICS
    }
}

/// `TypeIntrinsics.instanceOf`: the call that replaces `instanceof`, leaving an `int` 0/1.
pub(crate) fn instance_check(role: TypeCheckRole) -> IntrinsicCall {
    match role {
        TypeCheckRole::MutableCollection(kind) => IntrinsicCall {
            arity: None,
            name: format!("isMutable{}", suffix(kind)),
            descriptor: "(Ljava/lang/Object;)Z".to_owned(),
        },
        TypeCheckRole::FunctionOfArity(arity) => IntrinsicCall {
            arity: Some(arity),
            name: "isFunctionOfArity".to_owned(),
            descriptor: "(Ljava/lang/Object;I)Z".to_owned(),
        },
    }
}

/// `TypeIntrinsics.checkcast` for a non-safe cast: the call made before the `checkcast`, and
/// whether that `checkcast` is still written (a mutable collection's `asMutableX` already
/// returns the JVM interface).
pub(crate) fn cast(role: TypeCheckRole) -> (IntrinsicCall, bool) {
    match role {
        TypeCheckRole::MutableCollection(kind) => (
            IntrinsicCall {
                arity: None,
                name: format!("asMutable{}", suffix(kind)),
                descriptor: format!("(Ljava/lang/Object;)L{};", jvm_interface(kind)),
            },
            false,
        ),
        TypeCheckRole::FunctionOfArity(arity) => (
            IntrinsicCall {
                arity: Some(arity),
                name: "beforeCheckcastToFunctionOfArity".to_owned(),
                descriptor: "(Ljava/lang/Object;I)Ljava/lang/Object;".to_owned(),
            },
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_faces_have_kotlinc_markers() {
        let entry = |mutable| MappedCollection {
            kind: CollectionKind::MapEntry,
            mutable,
        };
        assert_eq!(
            collection_marker(entry(false)),
            "kotlin/jvm/internal/markers/KMappedMarker"
        );
        assert_eq!(
            collection_marker(entry(true)),
            "kotlin/jvm/internal/markers/KMutableMap$Entry"
        );
    }

    #[test]
    fn calls_follow_kotlinc_type_intrinsics() {
        let list = TypeCheckRole::MutableCollection(CollectionKind::List);
        assert_eq!(instance_check(list).name, "isMutableList");
        assert_eq!(
            cast(list),
            (
                IntrinsicCall {
                    arity: None,
                    name: "asMutableList".to_owned(),
                    descriptor: "(Ljava/lang/Object;)Ljava/util/List;".to_owned(),
                },
                false
            )
        );
        let function = TypeCheckRole::FunctionOfArity(1);
        assert_eq!(instance_check(function).arity, Some(1));
        assert!(cast(function).1);
    }
}
