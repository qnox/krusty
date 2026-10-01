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
    /// Convert a provider-normalized Kotlin compile-time constant into common IR. The semantic
    /// type chooses the exact constant identity; a target backend chooses its physical carrier.
    pub(crate) fn from_library_constant(constant: &crate::libraries::LibraryConst) -> Option<Self> {
        use crate::libraries::LibConst;

        Some(match (&constant.value, constant.ty.canonical_semantic()) {
            (LibConst::Int(value), Ty::Boolean) => Self::Boolean(*value != 0),
            (LibConst::Int(value), Ty::Char) => Self::Char(*value as u16),
            (LibConst::Int(value), Ty::Byte) => Self::Byte(*value as i8),
            (LibConst::Int(value), Ty::Short) => Self::Short(*value as i16),
            (LibConst::Int(value), Ty::Int) => Self::Int(*value),
            (LibConst::Int(value), Ty::UByte) => Self::UByte(*value as u8),
            (LibConst::Int(value), Ty::UShort) => Self::UShort(*value as u16),
            (LibConst::Int(value), Ty::UInt) => Self::UInt(*value as u32),
            (LibConst::Long(value), Ty::Long) => Self::Long(*value),
            (LibConst::Long(value), Ty::ULong) => Self::ULong(*value as u64),
            (LibConst::Float(value), Ty::Float) => Self::Float(*value),
            (LibConst::Double(value), Ty::Double) => Self::Double(*value),
            (LibConst::Str(value), Ty::String) => Self::String(value.clone()),
            _ => return None,
        })
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{LibConst, LibraryConst};

    fn convert(ty: Ty, value: LibConst) -> Option<IrConst> {
        IrConst::from_library_constant(&LibraryConst { ty, value })
    }

    #[test]
    fn library_constants_keep_their_semantic_identity() {
        assert_eq!(
            convert(Ty::Byte, LibConst::Int(255)),
            Some(IrConst::Byte(-1))
        );
        assert_eq!(
            convert(Ty::UInt, LibConst::Int(-1)),
            Some(IrConst::UInt(u32::MAX))
        );
        assert_eq!(
            convert(Ty::ULong, LibConst::Long(-1)),
            Some(IrConst::ULong(u64::MAX))
        );
        assert_eq!(
            convert(Ty::String, LibConst::Str(KtString::from("OK"))),
            Some(IrConst::String(KtString::from("OK")))
        );
    }

    #[test]
    fn incompatible_library_constant_shape_is_rejected() {
        assert_eq!(convert(Ty::Long, LibConst::Int(1)), None);
        assert_eq!(convert(Ty::String, LibConst::Int(1)), None);
    }
}
