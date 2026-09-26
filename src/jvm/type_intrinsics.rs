//! kotlinc's `TypeIntrinsics`: the type checks and casts a JVM `instanceof`/`checkcast` cannot
//! express.
//!
//! A mutable Kotlin collection shares its JVM interface with its read-only face, so `x is
//! MutableList<*>` asks `kotlin.jvm.internal.TypeIntrinsics.isMutableList`, which also rejects a
//! Kotlin read-only implementation. A function type erases to `FunctionN`, which every lambda of
//! any arity may implement through `FunctionBase`, so `x is (Int) -> Int` asks
//! `isFunctionOfArity(x, 1)`. Both apply to a type operation's semantic target: a written one, and
//! a reified argument once it is substituted.

use crate::types::{Ty, TypeName};

const TYPE_INTRINSICS: &str = "kotlin/jvm/internal/TypeIntrinsics";

/// A type-operation target that kotlinc routes through `TypeIntrinsics`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TypeIntrinsic {
    /// A mutable collection classifier.
    Mutable(MutableCollection),
    /// A non-suspend function type of this arity (receiver and context parameters included).
    FunctionOfArity(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MutableCollection {
    Iterator,
    Iterable,
    Collection,
    List,
    ListIterator,
    Set,
    Map,
    MapEntry,
}

/// Kotlin's mutable collection classifiers and what kotlinc's `TypeIntrinsics` calls each one.
const MUTABLE_COLLECTIONS: [(&str, MutableCollection); 9] = [
    (
        "kotlin/collections/MutableIterator",
        MutableCollection::Iterator,
    ),
    (
        "kotlin/collections/MutableIterable",
        MutableCollection::Iterable,
    ),
    (
        "kotlin/collections/MutableCollection",
        MutableCollection::Collection,
    ),
    ("kotlin/collections/MutableList", MutableCollection::List),
    (
        "kotlin/collections/MutableListIterator",
        MutableCollection::ListIterator,
    ),
    ("kotlin/collections/MutableSet", MutableCollection::Set),
    ("kotlin/collections/MutableMap", MutableCollection::Map),
    (
        "kotlin/collections/MutableMap.MutableEntry",
        MutableCollection::MapEntry,
    ),
    (
        "kotlin/collections/MutableMap$MutableEntry",
        MutableCollection::MapEntry,
    ),
];

/// Kotlin's read-only collection classifiers, which share their JVM interface with a mutable face.
const READ_ONLY_COLLECTIONS: [&str; 9] = [
    "kotlin/collections/Iterator",
    "kotlin/collections/Iterable",
    "kotlin/collections/Collection",
    "kotlin/collections/List",
    "kotlin/collections/ListIterator",
    "kotlin/collections/Set",
    "kotlin/collections/Map",
    "kotlin/collections/Map.Entry",
    "kotlin/collections/Map$Entry",
];

const MAPPED_MARKER: &str = "kotlin/jvm/internal/markers/KMappedMarker";

/// kotlinc's `KOTLIN_MARKER_INTERFACES`: the marker interface a class implementing the Kotlin
/// collection classifier `name` also implements, which is what the `TypeIntrinsics` checks read at
/// run time. A read-only face is a `KMappedMarker`; a mutable one is its own `KMutableX`.
pub(crate) fn collection_marker(name: TypeName) -> Option<&'static str> {
    if let Some(collection) = mutable_collection(name) {
        return Some(collection.marker());
    }
    READ_ONLY_COLLECTIONS
        .iter()
        .any(|candidate| name.matches(candidate))
        .then_some(MAPPED_MARKER)
}

/// The mutable collection a Kotlin classifier names, if any.
pub(crate) fn mutable_collection(name: TypeName) -> Option<MutableCollection> {
    MUTABLE_COLLECTIONS
        .iter()
        .find(|(candidate, _)| name.matches(candidate))
        .map(|(_, collection)| *collection)
}

impl TypeIntrinsic {
    /// kotlinc's choice for the target `ty` of an `is` or `as` (nullability aside).
    pub(crate) fn of(ty: Ty) -> Option<Self> {
        match ty.non_null() {
            Ty::Obj(name, arguments) => mutable_collection(name)
                .map(Self::Mutable)
                .or_else(|| function_classifier_arity(name, arguments).map(Self::FunctionOfArity)),
            Ty::Fun(signature) if !signature.suspend => u8::try_from(signature.params.len())
                .ok()
                .map(Self::FunctionOfArity),
            _ => None,
        }
    }
}

/// A written `kotlin.FunctionN<P1, …, R>` classifier (kotlinc's `KOTLIN_FUNCTION_INTERFACE_REGEX`
/// match). Its arity is its declaration's: every type parameter but the result is a parameter.
fn function_classifier_arity(name: TypeName, arguments: &[Ty]) -> Option<u8> {
    let arity = u8::try_from(arguments.len().checked_sub(1)?).ok()?;
    name.matches(&format!("kotlin/Function{arity}"))
        .then_some(arity)
}

impl MutableCollection {
    fn suffix(self) -> &'static str {
        match self {
            Self::Iterator => "Iterator",
            Self::Iterable => "Iterable",
            Self::Collection => "Collection",
            Self::List => "List",
            Self::ListIterator => "ListIterator",
            Self::Set => "Set",
            Self::Map => "Map",
            Self::MapEntry => "MapEntry",
        }
    }

    fn marker(self) -> &'static str {
        match self {
            Self::Iterator => "kotlin/jvm/internal/markers/KMutableIterator",
            Self::Iterable => "kotlin/jvm/internal/markers/KMutableIterable",
            Self::Collection => "kotlin/jvm/internal/markers/KMutableCollection",
            Self::List => "kotlin/jvm/internal/markers/KMutableList",
            Self::ListIterator => "kotlin/jvm/internal/markers/KMutableListIterator",
            Self::Set => "kotlin/jvm/internal/markers/KMutableSet",
            Self::Map => "kotlin/jvm/internal/markers/KMutableMap",
            Self::MapEntry => "kotlin/jvm/internal/markers/KMutableMap$Entry",
        }
    }

    fn jvm_interface(self) -> &'static str {
        match self {
            Self::Iterator => "java/util/Iterator",
            Self::Iterable => "java/lang/Iterable",
            Self::Collection => "java/util/Collection",
            Self::List => "java/util/List",
            Self::ListIterator => "java/util/ListIterator",
            Self::Set => "java/util/Set",
            Self::Map => "java/util/Map",
            Self::MapEntry => "java/util/Map$Entry",
        }
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

impl TypeIntrinsic {
    /// `TypeIntrinsics.instanceOf`: the call that replaces `instanceof`, leaving an `int` 0/1.
    pub(crate) fn instance_check(self) -> IntrinsicCall {
        match self {
            Self::Mutable(collection) => IntrinsicCall {
                arity: None,
                name: format!("isMutable{}", collection.suffix()),
                descriptor: "(Ljava/lang/Object;)Z".to_owned(),
            },
            Self::FunctionOfArity(arity) => IntrinsicCall {
                arity: Some(arity),
                name: "isFunctionOfArity".to_owned(),
                descriptor: "(Ljava/lang/Object;I)Z".to_owned(),
            },
        }
    }

    /// `TypeIntrinsics.checkcast` for a non-safe cast: the call made before the `checkcast`, and
    /// whether that `checkcast` is still written (a mutable collection's `asMutableX` already
    /// returns the JVM interface).
    pub(crate) fn cast(self) -> (IntrinsicCall, bool) {
        match self {
            Self::Mutable(collection) => (
                IntrinsicCall {
                    arity: None,
                    name: format!("asMutable{}", collection.suffix()),
                    descriptor: format!("(Ljava/lang/Object;)L{};", collection.jvm_interface()),
                },
                false,
            ),
            Self::FunctionOfArity(arity) => (
                IntrinsicCall {
                    arity: Some(arity),
                    name: "beforeCheckcastToFunctionOfArity".to_owned(),
                    descriptor: "(Ljava/lang/Object;I)Ljava/lang/Object;".to_owned(),
                },
                true,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{intern_fnsig, type_name, FnSig};

    #[test]
    fn mutable_collections_and_plain_function_types_have_intrinsics() {
        let entry = Ty::Obj(type_name("kotlin/collections/MutableMap.MutableEntry"), &[]);
        assert_eq!(
            TypeIntrinsic::of(Ty::nullable(entry)),
            Some(TypeIntrinsic::Mutable(MutableCollection::MapEntry))
        );
        assert_eq!(
            TypeIntrinsic::of(Ty::Obj(type_name("kotlin/collections/List"), &[])),
            None
        );
        let function = |suspend| {
            Ty::Fun(intern_fnsig(FnSig {
                params: vec![Ty::Int, Ty::String],
                ret: Ty::Int,
                context_count: 0,
                has_receiver: true,
                suspend,
            }))
        };
        assert_eq!(
            TypeIntrinsic::of(function(false)),
            Some(TypeIntrinsic::FunctionOfArity(2))
        );
        assert_eq!(TypeIntrinsic::of(function(true)), None);
    }

    #[test]
    fn collection_supertypes_have_kotlinc_markers() {
        let marker = |name| collection_marker(type_name(name));
        assert_eq!(
            marker("kotlin/collections/Map.Entry"),
            Some("kotlin/jvm/internal/markers/KMappedMarker")
        );
        assert_eq!(
            marker("kotlin/collections/MutableMap.MutableEntry"),
            Some("kotlin/jvm/internal/markers/KMutableMap$Entry")
        );
        assert_eq!(marker("kotlin/collections/AbstractList"), None);
    }

    #[test]
    fn calls_follow_kotlinc_type_intrinsics() {
        let list = TypeIntrinsic::Mutable(MutableCollection::List);
        assert_eq!(list.instance_check().name, "isMutableList");
        assert_eq!(
            list.cast(),
            (
                IntrinsicCall {
                    arity: None,
                    name: "asMutableList".to_owned(),
                    descriptor: "(Ljava/lang/Object;)Ljava/util/List;".to_owned(),
                },
                false
            )
        );
        let function = TypeIntrinsic::FunctionOfArity(1);
        assert_eq!(function.instance_check().arity, Some(1));
        assert!(function.cast().1);
    }
}
