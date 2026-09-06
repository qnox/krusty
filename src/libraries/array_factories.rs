//! Exact semantic identities for compiler-provided Kotlin array factories.
//!
//! These declarations have no ordinary callable implementation. Symbol providers attach the
//! resulting kind while normalizing the selected `kotlin` package declaration; checked FIR and
//! common lowering then consume that identity without consulting the legacy synthetic-IR registry.

use crate::types::{ArrayFactoryKind, Ty};

pub(crate) fn kotlin_array_factory_kind(name: &str) -> Option<ArrayFactoryKind> {
    Some(match name {
        "intArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::Int),
        "longArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::Long),
        "doubleArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::Double),
        "floatArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::Float),
        "booleanArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::Boolean),
        "charArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::Char),
        "byteArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::Byte),
        "shortArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::Short),
        "ubyteArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::UByte),
        "ushortArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::UShort),
        "uintArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::UInt),
        "ulongArrayOf" => ArrayFactoryKind::PrimitiveVararg(Ty::ULong),
        "IntArray" => ArrayFactoryKind::PrimitiveSize(Ty::Int),
        "LongArray" => ArrayFactoryKind::PrimitiveSize(Ty::Long),
        "DoubleArray" => ArrayFactoryKind::PrimitiveSize(Ty::Double),
        "FloatArray" => ArrayFactoryKind::PrimitiveSize(Ty::Float),
        "BooleanArray" => ArrayFactoryKind::PrimitiveSize(Ty::Boolean),
        "CharArray" => ArrayFactoryKind::PrimitiveSize(Ty::Char),
        "ByteArray" => ArrayFactoryKind::PrimitiveSize(Ty::Byte),
        "ShortArray" => ArrayFactoryKind::PrimitiveSize(Ty::Short),
        "UByteArray" => ArrayFactoryKind::PrimitiveSize(Ty::UByte),
        "UShortArray" => ArrayFactoryKind::PrimitiveSize(Ty::UShort),
        "UIntArray" => ArrayFactoryKind::PrimitiveSize(Ty::UInt),
        "ULongArray" => ArrayFactoryKind::PrimitiveSize(Ty::ULong),
        "arrayOf" => ArrayFactoryKind::ReferenceVararg,
        "Array" => ArrayFactoryKind::ReferenceSize,
        "emptyArray" => ArrayFactoryKind::EmptyReference,
        "arrayOfNulls" => ArrayFactoryKind::NullableReference,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_declared_kotlin_array_factories() {
        assert_eq!(
            kotlin_array_factory_kind("uintArrayOf"),
            Some(ArrayFactoryKind::PrimitiveVararg(Ty::UInt))
        );
        assert_eq!(
            kotlin_array_factory_kind("Array"),
            Some(ArrayFactoryKind::ReferenceSize)
        );
        assert_eq!(kotlin_array_factory_kind("arrayOfAnything"), None);
    }
}
