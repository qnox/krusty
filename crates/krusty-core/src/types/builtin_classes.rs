//! The builtin classes a non-JVM target carries in representations of its own.

use super::Ty;

/// `String`, the primitives and the unsigned integers: each the only instance type of its own
/// final classifier, so a target that carries its values in a string or a box of its own answers
/// for them wherever the classifiers above them are named.
pub const REPRESENTED_BUILTINS: [Ty; 13] = [
    Ty::String,
    Ty::Boolean,
    Ty::Byte,
    Ty::Short,
    Ty::Char,
    Ty::Int,
    Ty::Long,
    Ty::Float,
    Ty::Double,
    Ty::UByte,
    Ty::UShort,
    Ty::UInt,
    Ty::ULong,
];
