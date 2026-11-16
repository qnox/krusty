//! The builtin operations kotlinc's constant evaluator applies to constant values. Both the
//! constant-context evaluator (annotation arguments and `const val` initializers) and body folding
//! evaluate them here, so each operation has one definition; each caller keeps only its own rule
//! for when an operation is eligible.

use crate::kt_string::KtStringBuf;
use crate::libraries::{LibConst, LibraryConst};
use crate::types::Ty;

use super::FirConstant;

/// A library constant's value as checked FIR carries it: the constant's declared type decides how
/// its stored integer reads (a `Boolean`, a `Char`, or an unsigned value).
pub fn library_constant_value(constant: &LibraryConst) -> FirConstant {
    match &constant.value {
        LibConst::Int(value) => match constant.ty.non_null() {
            Ty::Boolean => FirConstant::Boolean(*value != 0),
            Ty::Char => FirConstant::Char(*value as u16),
            Ty::UInt | Ty::UByte | Ty::UShort => FirConstant::UInt(i64::from(*value as u32)),
            _ => FirConstant::Int(i64::from(*value)),
        },
        LibConst::Long(value) => match constant.ty.non_null() {
            Ty::ULong => FirConstant::ULong(*value),
            _ => FirConstant::Long(*value),
        },
        LibConst::Float(value) => FirConstant::Float(*value),
        LibConst::Double(value) => FirConstant::Double(*value),
        LibConst::Str(value) => FirConstant::String(value.clone()),
    }
}

/// The magnitude of an unsigned constant of type `ty`, read at that type's width.
pub fn unsigned_constant(value: &FirConstant, ty: Ty) -> Option<u64> {
    Some(match (value, ty.non_null().canonical_semantic()) {
        (FirConstant::UInt(value), Ty::UByte) => u64::from(*value as u8),
        (FirConstant::UInt(value), Ty::UShort) => u64::from(*value as u16),
        (FirConstant::UInt(value), Ty::UInt) => u64::from(*value as u32),
        (FirConstant::ULong(value), Ty::ULong) => *value as u64,
        _ => return None,
    })
}

/// Append the `toString()` text of a constant of type `ty`, as a template or `toString()` does.
pub fn push_constant_text(value: &FirConstant, ty: Ty, text: &mut KtStringBuf) -> Option<()> {
    match value {
        FirConstant::String(value) => text.push_kt(value),
        FirConstant::Null => text.push_str("null"),
        FirConstant::Boolean(value) => text.push_str(if *value { "true" } else { "false" }),
        FirConstant::Char(value) => text.push_unit(*value),
        FirConstant::Int(value) | FirConstant::Long(value) => text.push_str(&value.to_string()),
        FirConstant::UInt(_) | FirConstant::ULong(_) => {
            text.push_str(&unsigned_constant(value, ty)?.to_string())
        }
        FirConstant::Float(value) => crate::kt_string::push_f32(*value, text)?,
        FirConstant::Double(value) => crate::kt_string::push_f64(*value, text)?,
    }
    Some(())
}

/// `String.length` of a constant string: its UTF-16 length.
pub fn constant_string_length(value: &FirConstant) -> Option<i64> {
    let FirConstant::String(text) = value else {
        return None;
    };
    i64::try_from(text.len_utf16()).ok()
}
