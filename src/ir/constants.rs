//! Compile-time constant values carried in common IR.
//!
//! A constant keeps the checked IDENTITY of its type, not a representation: an unsigned constant
//! stands for the value it names, never for the signed carrier a particular backend happens to
//! store it in.

use super::*;

/// A compile-time constant (`IrConst` in Kotlin IR).
#[derive(Clone, Debug, PartialEq)]
pub enum IrConst {
    Boolean(bool),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    /// A Kotlin `Char` — one UTF-16 code UNIT, not a code point. Lone surrogates (D800..DFFF) are
    /// legal `Char` values (`Char.MIN_HIGH_SURROGATE`), so this cannot be a Rust `char`: converting
    /// through `char::from_u32` rejects them and silently folds them to NUL.
    Char(u16),
    /// A Kotlin `String` — a sequence of UTF-16 code units. Same reason as `Char`: `"\uD800"` and
    /// `"😀"` have no Rust `String` spelling one code unit at a time.
    String(crate::kt_string::KtString),
    /// An unsigned constant, as the VALUE it stands for: `200u` is 200 here, never the byte -56
    /// that a JVM carries a `UByte` in.
    ///
    /// These exist because the unsigned type is the constant's checked IDENTITY and a backend
    /// cannot choose a representation for what it cannot see. Folding them into `Int` lost that:
    /// `value_ty` then answers `Int` from the constant's shape, and every backend inherited
    /// whatever width the number happened to carry. Which primitive holds the value is a
    /// representation decision, and representation belongs to backends. Matching widths do not
    /// make `UInt` semantically identical to `Int`, or `ULong` to `Long`.
    UByte(u8),
    UShort(u16),
    UInt(u32),
    ULong(u64),
    Null,
}

impl IrConst {
    pub fn zero_for_value_type(ty: Ty) -> IrConst {
        match ty.canonical_semantic() {
            Ty::Boolean => IrConst::Boolean(false),
            Ty::Byte => IrConst::Byte(0),
            Ty::UByte => IrConst::UByte(0),
            Ty::Short => IrConst::Short(0),
            Ty::UShort => IrConst::UShort(0),
            Ty::Int => IrConst::Int(0),
            Ty::UInt => IrConst::UInt(0),
            Ty::Long => IrConst::Long(0),
            Ty::ULong => IrConst::ULong(0),
            Ty::Float => IrConst::Float(0.0),
            Ty::Double => IrConst::Double(0.0),
            Ty::Char => IrConst::Char(0),
            _ => IrConst::Null,
        }
    }
}
