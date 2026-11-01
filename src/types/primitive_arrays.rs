//! The primitive specialized array classes (`IntArray`, `UByteArray`, ...) and the element each
//! holds, with the lookups between a class, its creator functions, and its element type.

use super::{ClassifierDeclarationName, InternalName, Ty, TypeName};

/// Every primitive specialized array class and the element it holds — THE inventory, in one place.
///
/// An unsigned array keeps its unsigned element so its value-class identity survives; it still
/// erases to the signed primitive array descriptor (`[I`), which is [`crate::jvm::names`]'s job and
/// not this table's. Anything needing the set of these classes iterates here rather than writing the
/// names out again: every copy that has existed eventually disagreed with this one, and the ones
/// that omitted `UByteArray` and `UShortArray` emitted `int[]` for a `byte[]`.
const PRIM_ARRAY_CLASSES: [(&str, Ty); 12] = [
    ("kotlin/IntArray", Ty::Int),
    ("kotlin/LongArray", Ty::Long),
    ("kotlin/ShortArray", Ty::Short),
    ("kotlin/ByteArray", Ty::Byte),
    ("kotlin/BooleanArray", Ty::Boolean),
    ("kotlin/CharArray", Ty::Char),
    ("kotlin/FloatArray", Ty::Float),
    ("kotlin/DoubleArray", Ty::Double),
    ("kotlin/UByteArray", Ty::UByte),
    ("kotlin/UShortArray", Ty::UShort),
    ("kotlin/UIntArray", Ty::UInt),
    ("kotlin/ULongArray", Ty::ULong),
];

/// The declaration owned by an admitted Kotlin array family, from the canonical inventory.
pub(crate) fn array_declaration_name(internal: TypeName) -> Option<ClassifierDeclarationName> {
    let name = if internal == super::wk::array() {
        "Array"
    } else {
        PRIM_ARRAY_CLASSES
            .iter()
            .find(|(name, _)| internal.internal_matches(name))?
            .0
            .strip_prefix("kotlin/")
            .expect("the primitive array inventory declares Kotlin classes")
    };
    Some(ClassifierDeclarationName::TopLevel {
        package: super::wk::kotlin_package(),
        name: name.into(),
    })
}

/// The element type of a primitive specialized array class (`kotlin/IntArray` → `Int`), or `None`
/// for a non-primitive-array name. A lookup in [`PRIM_ARRAY_CLASSES`], which is the canonical table
/// — [`Ty::array_elem`], the constructor, and the backend descriptor logic all route through this
/// rather than each carrying their own copy.
pub fn prim_array_element(internal: impl InternalName) -> Option<Ty> {
    // Every name in the table ends in `Array`; most names asked about are not arrays at all.
    if !internal.final_segment().ends_with("Array") {
        return None;
    }
    PRIM_ARRAY_CLASSES
        .iter()
        .find(|(name, _)| internal.internal_matches(name))
        .map(|(_, element)| *element)
}

/// Element type fixed by a specialized primitive-array creator. Both the class-shaped size
/// constructor (`IntArray`) and its vararg factory (`intArrayOf`) name the same semantic element.
/// Resolution, signature inference, and intrinsic emission share this table.
pub fn primitive_array_creator_element(name: &str) -> Option<Ty> {
    Some(match name {
        "IntArray" | "intArrayOf" => Ty::Int,
        "LongArray" | "longArrayOf" => Ty::Long,
        "ShortArray" | "shortArrayOf" => Ty::Short,
        "ByteArray" | "byteArrayOf" => Ty::Byte,
        "BooleanArray" | "booleanArrayOf" => Ty::Boolean,
        "CharArray" | "charArrayOf" => Ty::Char,
        "FloatArray" | "floatArrayOf" => Ty::Float,
        "DoubleArray" | "doubleArrayOf" => Ty::Double,
        "UByteArray" | "ubyteArrayOf" => Ty::UByte,
        "UShortArray" | "ushortArrayOf" => Ty::UShort,
        "UIntArray" | "uintArrayOf" => Ty::UInt,
        "ULongArray" | "ulongArrayOf" => Ty::ULong,
        _ => return None,
    })
}

/// The primitive specialized array class name for a primitive element (`Int` → `kotlin/IntArray`), or
/// `None` for a reference element (which lives in a boxed `Array<T>`). Inverse of [`prim_array_element`].
pub fn prim_array_name(elem: Ty) -> Option<&'static str> {
    PRIM_ARRAY_CLASSES
        .iter()
        .find(|(_, element)| *element == elem)
        .map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::type_name;

    #[test]
    fn primitive_array_elements_are_found_by_every_name_spelling() {
        for (name, element) in PRIM_ARRAY_CLASSES {
            assert_eq!(prim_array_element(name), Some(element), "{name}");
            assert_eq!(prim_array_element(&name.to_string()), Some(element));
            assert_eq!(prim_array_element(type_name(name)), Some(element));
        }
        for name in [
            "kotlin/Array",
            "kotlin/Int",
            "sample/IntArray",
            "IntArray",
            "kotlin/String",
        ] {
            assert_eq!(prim_array_element(name), None, "{name}");
            assert_eq!(prim_array_element(type_name(name)), None, "{name}");
        }
    }
}
