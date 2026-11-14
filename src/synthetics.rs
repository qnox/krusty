//! Registry of compiler-provided array constructors (`IntArray(n)`, `Array(n) { … }`).
//!
//! These constructors have no ordinary classpath body. Resolution records their stable identity
//! and [`SyntheticKind`]; checked FIR owns their semantics and common lowering constructs the IR.
//! The array factory functions (`arrayOf`, `intArrayOf`, `emptyArray`, `arrayOfNulls`, …) are
//! ordinary `kotlin` package declarations: they are selected from metadata like any other call.

use crate::types::Ty;

pub use crate::types::ArrayFactoryKind as SyntheticKind;

/// One compiler-provided declaration and the semantic operation it denotes.
pub struct Synthetic {
    pub fqn: &'static str,
    pub name: &'static str,
    pub(crate) kind: SyntheticKind,
}

/// The synthetic whose source call name is `name`, or `None`. Has priority over the classpath; the
/// caller is responsible for honoring user-declared shadowing first.
pub fn lookup(name: &str) -> Option<&'static Synthetic> {
    TABLE.iter().find(|s| s.name == name)
}

pub(crate) fn by_kind(kind: SyntheticKind) -> Option<&'static Synthetic> {
    TABLE.iter().find(|synthetic| synthetic.kind == kind)
}

/// The type parameter a reference-array creator materializes at run time: the `reified T` of
/// `arrayOf`, `emptyArray` and `arrayOfNulls`, and the `T` of `kotlin.Array` its constructor
/// creates. A primitive array creator has no type parameter.
pub(crate) fn reified_type_parameter(kind: SyntheticKind) -> Option<&'static str> {
    match kind {
        SyntheticKind::ReferenceVararg
        | SyntheticKind::ReferenceSize
        | SyntheticKind::EmptyReference
        | SyntheticKind::NullableReference => Some("T"),
        SyntheticKind::PrimitiveVararg(_) | SyntheticKind::PrimitiveSize(_) => None,
    }
}

const fn syn(fqn: &'static str, name: &'static str, kind: SyntheticKind) -> Synthetic {
    Synthetic { fqn, name, kind }
}

static TABLE: &[Synthetic] = &[
    // Primitive size constructors — `IntArray(n)` / `IntArray(n) { i -> e }`.
    syn(
        "kotlin/IntArray",
        "IntArray",
        SyntheticKind::PrimitiveSize(Ty::Int),
    ),
    syn(
        "kotlin/LongArray",
        "LongArray",
        SyntheticKind::PrimitiveSize(Ty::Long),
    ),
    syn(
        "kotlin/DoubleArray",
        "DoubleArray",
        SyntheticKind::PrimitiveSize(Ty::Double),
    ),
    syn(
        "kotlin/FloatArray",
        "FloatArray",
        SyntheticKind::PrimitiveSize(Ty::Float),
    ),
    syn(
        "kotlin/BooleanArray",
        "BooleanArray",
        SyntheticKind::PrimitiveSize(Ty::Boolean),
    ),
    syn(
        "kotlin/CharArray",
        "CharArray",
        SyntheticKind::PrimitiveSize(Ty::Char),
    ),
    syn(
        "kotlin/ByteArray",
        "ByteArray",
        SyntheticKind::PrimitiveSize(Ty::Byte),
    ),
    syn(
        "kotlin/ShortArray",
        "ShortArray",
        SyntheticKind::PrimitiveSize(Ty::Short),
    ),
    // The reference-array constructor `Array(n) { i -> e }`. Unsigned size constructors
    // (`UIntArray(n)`, …) are value-class declarations in metadata, not entries here.
    syn("kotlin/Array", "Array", SyntheticKind::ReferenceSize),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_each_registered_synthetic_by_source_name() {
        // Every TABLE entry is discoverable by its source call name, and the returned identity
        // (fqn/name) matches the entry — the identity shared with the JVM intrinsic registry.
        for s in TABLE {
            let got = lookup(s.name).expect("registered name must resolve");
            assert_eq!(got.name, s.name);
            assert_eq!(got.fqn, s.fqn);
        }
    }

    #[test]
    fn lookup_declines_unknown_name() {
        assert!(lookup("nope").is_none());
        assert!(lookup("").is_none());
        // A real classpath function that is deliberately NOT synthetic must not match.
        assert!(lookup("listOf").is_none());
        assert!(lookup("println").is_none());
        // An unsigned array is a value class whose size constructor is declared in metadata.
        assert!(lookup("UIntArray").is_none());
        assert!(lookup("ULongArray").is_none());
        // The array factory functions are ordinary `kotlin` declarations selected from metadata.
        for name in [
            "arrayOf",
            "intArrayOf",
            "uintArrayOf",
            "emptyArray",
            "arrayOfNulls",
        ] {
            assert!(
                lookup(name).is_none(),
                "{name} must resolve through metadata"
            );
        }
    }

    #[test]
    fn lookup_covers_the_documented_constructor_families() {
        // Primitive size constructors, plus the reference-array constructor.
        for name in ["IntArray", "LongArray", "Array"] {
            assert!(lookup(name).is_some(), "{name} should be synthetic");
        }
    }

    #[test]
    fn creator_kind_carries_the_element_without_parsing_the_name() {
        assert_eq!(
            lookup("IntArray").map(|s| s.kind),
            Some(SyntheticKind::PrimitiveSize(Ty::Int))
        );
        assert_eq!(
            lookup("Array").map(|s| s.kind),
            Some(SyntheticKind::ReferenceSize)
        );
    }

    #[test]
    fn syn_constructs_the_expected_identity() {
        let s = syn(
            "kotlin/IntArray",
            "IntArray",
            SyntheticKind::PrimitiveSize(Ty::Int),
        );
        assert_eq!(s.fqn, "kotlin/IntArray");
        assert_eq!(s.name, "IntArray");
    }

    #[test]
    fn registered_source_names_are_unique() {
        // lookup() returns the FIRST match; a duplicate source name would silently shadow — assert none.
        let mut names: Vec<&str> = TABLE.iter().map(|s| s.name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "synthetic source names must be unique");
    }
}
