//! Registry of compiler-provided array factory declarations.
//!
//! These declarations have no ordinary callable body in classpath metadata. Resolution selects a
//! semantic factory kind; checked FIR and target lowering realize that kind without retaining an
//! AST-lowering callback here.
//!
//! Functions that DO have a real (inline) classpath body — `require`/`check`/`println`/`listOf`/… — are
//! deliberately NOT here; they resolve through the classpath, with signatures recovered from `@Metadata`.

use crate::types::{Ty, TypeName};

pub use crate::types::ArrayFactoryKind as SyntheticKind;

/// One compiler-provided declaration and the semantic factory operation it denotes.
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

/// Resolve a compiler-provided declaration from an already-bound package qualifier. This is the
/// qualified counterpart of [`lookup`]: callers supply semantic package identity, so source syntax
/// such as `kotlin.arrayOf` and an imported `arrayOf` select the same registry entry.
pub fn lookup_qualified(package: TypeName, name: &str) -> Option<&'static Synthetic> {
    TABLE.iter().find(|synthetic| {
        synthetic.name == name
            && synthetic
                .fqn
                .rsplit_once('/')
                .is_some_and(|(owner, declared)| declared == name && package.matches(owner))
    })
}

pub(crate) fn by_kind(kind: SyntheticKind) -> Option<&'static Synthetic> {
    TABLE.iter().find(|synthetic| synthetic.kind == kind)
}

const fn syn(fqn: &'static str, name: &'static str, kind: SyntheticKind) -> Synthetic {
    Synthetic { fqn, name, kind }
}

static TABLE: &[Synthetic] = &[
    // Primitive vararg literals — `intArrayOf(1, 2, 3): IntArray`.
    syn(
        "kotlin/intArrayOf",
        "intArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::Int),
    ),
    syn(
        "kotlin/longArrayOf",
        "longArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::Long),
    ),
    syn(
        "kotlin/doubleArrayOf",
        "doubleArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::Double),
    ),
    syn(
        "kotlin/floatArrayOf",
        "floatArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::Float),
    ),
    syn(
        "kotlin/booleanArrayOf",
        "booleanArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::Boolean),
    ),
    syn(
        "kotlin/charArrayOf",
        "charArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::Char),
    ),
    syn(
        "kotlin/byteArrayOf",
        "byteArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::Byte),
    ),
    syn(
        "kotlin/shortArrayOf",
        "shortArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::Short),
    ),
    // Unsigned vararg literals keep their semantic inline-class element. The backend owns the
    // corresponding signed primitive storage array (`UByteArray` -> `[B`, `UIntArray` -> `[I`).
    syn(
        "kotlin/ubyteArrayOf",
        "ubyteArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::UByte),
    ),
    syn(
        "kotlin/ushortArrayOf",
        "ushortArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::UShort),
    ),
    syn(
        "kotlin/uintArrayOf",
        "uintArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::UInt),
    ),
    syn(
        "kotlin/ulongArrayOf",
        "ulongArrayOf",
        SyntheticKind::PrimitiveVararg(Ty::ULong),
    ),
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
    // Unsigned size constructors use the same semantic elements as their vararg factories.
    syn(
        "kotlin/UByteArray",
        "UByteArray",
        SyntheticKind::PrimitiveSize(Ty::UByte),
    ),
    syn(
        "kotlin/UShortArray",
        "UShortArray",
        SyntheticKind::PrimitiveSize(Ty::UShort),
    ),
    syn(
        "kotlin/UIntArray",
        "UIntArray",
        SyntheticKind::PrimitiveSize(Ty::UInt),
    ),
    syn(
        "kotlin/ULongArray",
        "ULongArray",
        SyntheticKind::PrimitiveSize(Ty::ULong),
    ),
    // Reference creators.
    syn("kotlin/arrayOf", "arrayOf", SyntheticKind::ReferenceVararg),
    syn("kotlin/Array", "Array", SyntheticKind::ReferenceSize),
    syn(
        "kotlin/emptyArray",
        "emptyArray",
        SyntheticKind::EmptyReference,
    ),
    syn(
        "kotlin/arrayOfNulls",
        "arrayOfNulls",
        SyntheticKind::NullableReference,
    ),
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
    }

    #[test]
    fn lookup_covers_the_documented_creator_families() {
        // Vararg + size primitive families, plus the four reference creators.
        for name in [
            "intArrayOf",
            "longArrayOf",
            "IntArray",
            "LongArray",
            "uintArrayOf",
            "ulongArrayOf",
            "UIntArray",
            "ULongArray",
            "arrayOf",
            "Array",
            "emptyArray",
            "arrayOfNulls",
        ] {
            assert!(lookup(name).is_some(), "{name} should be synthetic");
        }
    }

    #[test]
    fn creator_kind_carries_the_element_without_parsing_the_name() {
        assert_eq!(
            lookup("intArrayOf").map(|s| s.kind),
            Some(SyntheticKind::PrimitiveVararg(Ty::Int))
        );
        assert_eq!(
            lookup("IntArray").map(|s| s.kind),
            Some(SyntheticKind::PrimitiveSize(Ty::Int))
        );
        assert_eq!(
            lookup("arrayOf").map(|s| s.kind),
            Some(SyntheticKind::ReferenceVararg)
        );
    }

    #[test]
    fn syn_constructs_the_expected_identity() {
        let s = syn(
            "kotlin/intArrayOf",
            "intArrayOf",
            SyntheticKind::PrimitiveVararg(Ty::Int),
        );
        assert_eq!(s.fqn, "kotlin/intArrayOf");
        assert_eq!(s.name, "intArrayOf");
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
