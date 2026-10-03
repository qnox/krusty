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
use crate::types::{CollectionKind, MappedCollection};

const TYPE_INTRINSICS: &str = "kotlin/jvm/internal/TypeIntrinsics";

const MAPPED_MARKER: &str = "kotlin/jvm/internal/markers/KMappedMarker";

/// The marker interface a suspend function value implements. `SuspendFunctionN` is not a class.
pub(crate) const SUSPEND_FUNCTION_MARKER: &str = "kotlin/coroutines/jvm/internal/SuspendFunction";

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

/// An instance test that is not a single `instanceof`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InstanceCheck {
    /// One `TypeIntrinsics` call, leaving an `int` 0/1.
    Call(IntrinsicCall),
    /// `instanceof SuspendFunction`, and `isFunctionOfArity` only when that marker matches.
    /// The number is the JVM arity.
    SuspendFunction { arity: u8 },
}

/// `TypeIntrinsics.instanceOf`: the check that replaces `instanceof`, leaving an `int` 0/1.
pub(crate) fn instance_check(role: TypeCheckRole) -> InstanceCheck {
    match role {
        TypeCheckRole::MutableCollection(kind) => InstanceCheck::Call(IntrinsicCall {
            arity: None,
            name: format!("isMutable{}", suffix(kind)),
            descriptor: "(Ljava/lang/Object;)Z".to_owned(),
        }),
        TypeCheckRole::FunctionOfArity(arity) => InstanceCheck::Call(function_arity_check(arity)),
        TypeCheckRole::SuspendFunctionOfArity(arity) => InstanceCheck::SuspendFunction { arity },
    }
}

/// `TypeIntrinsics.isFunctionOfArity(x, arity)`.
pub(crate) fn function_arity_check(arity: u8) -> IntrinsicCall {
    IntrinsicCall {
        arity: Some(arity),
        name: "isFunctionOfArity".to_owned(),
        descriptor: "(Ljava/lang/Object;I)Z".to_owned(),
    }
}

/// `TypeIntrinsics.checkcast` for a non-safe cast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CastCheck {
    /// The call made before the `checkcast`, and whether that `checkcast` is still written (a
    /// mutable collection's `asMutableX` already returns the JVM interface).
    Call {
        call: IntrinsicCall,
        checkcast: bool,
    },
    /// `as SuspendFunctionN` is a plain `checkcast` to `Function{arity}`. The marker test belongs
    /// to `is` and `as?`; kotlinc does not call `beforeCheckcastToFunctionOfArity` here.
    SuspendFunction { arity: u8 },
}

/// `TypeIntrinsics.checkcast` for a non-safe cast.
pub(crate) fn cast(role: TypeCheckRole) -> CastCheck {
    match role {
        TypeCheckRole::MutableCollection(kind) => CastCheck::Call {
            call: IntrinsicCall {
                arity: None,
                name: format!("asMutable{}", suffix(kind)),
                descriptor: format!("(Ljava/lang/Object;)L{};", jvm_interface(kind)),
            },
            checkcast: false,
        },
        TypeCheckRole::FunctionOfArity(arity) => CastCheck::Call {
            call: IntrinsicCall {
                arity: Some(arity),
                name: "beforeCheckcastToFunctionOfArity".to_owned(),
                descriptor: "(Ljava/lang/Object;I)Ljava/lang/Object;".to_owned(),
            },
            checkcast: true,
        },
        TypeCheckRole::SuspendFunctionOfArity(arity) => CastCheck::SuspendFunction { arity },
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
        assert_eq!(
            instance_check(list),
            InstanceCheck::Call(IntrinsicCall {
                arity: None,
                name: "isMutableList".to_owned(),
                descriptor: "(Ljava/lang/Object;)Z".to_owned(),
            })
        );
        assert_eq!(
            cast(list),
            CastCheck::Call {
                call: IntrinsicCall {
                    arity: None,
                    name: "asMutableList".to_owned(),
                    descriptor: "(Ljava/lang/Object;)Ljava/util/List;".to_owned(),
                },
                checkcast: false,
            }
        );
        let function = TypeCheckRole::FunctionOfArity(1);
        assert_eq!(
            instance_check(function),
            InstanceCheck::Call(function_arity_check(1))
        );
        assert!(matches!(
            cast(function),
            CastCheck::Call {
                checkcast: true,
                ..
            }
        ));
        let suspend = TypeCheckRole::SuspendFunctionOfArity(1);
        assert_eq!(
            instance_check(suspend),
            InstanceCheck::SuspendFunction { arity: 1 }
        );
        assert_eq!(cast(suspend), CastCheck::SuspendFunction { arity: 1 });
    }
}
