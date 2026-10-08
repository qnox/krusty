//! Native ABI carriers for checked Kotlin types.
//!
//! This is the single boundary that maps a semantic [`Ty`] to its physical Cranelift value and
//! calling-convention parameter. Lowering modules consume the carrier; they do not reconstruct it
//! from descriptors or declaration spellings.

use cranelift_codegen::ir::{types, AbiParam, Type};

use crate::types::Ty;

/// How a Kotlin type is carried in machine code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Carrier {
    /// `Unit` in return position: no value.
    Void,
    /// A machine scalar of the given Cranelift type; the flag says whether an argument of this type
    /// is sign- (true) or zero-extended when passed to the runtime's C ABI.
    Scalar(Type, bool),
    /// A `KRef`: an `i64` pointer into the collected heap, or null.
    Ref,
}

impl Carrier {
    pub(super) fn clif(self) -> Option<Type> {
        match self {
            Self::Void => None,
            Self::Scalar(ty, _) => Some(ty),
            Self::Ref => Some(types::I64),
        }
    }

    pub(super) fn abi_param(self) -> Option<AbiParam> {
        match self {
            Self::Void => None,
            Self::Scalar(ty, signed) if ty.bits() < 32 => Some(if signed {
                AbiParam::new(ty).sext()
            } else {
                AbiParam::new(ty).uext()
            }),
            Self::Scalar(ty, _) => Some(AbiParam::new(ty)),
            Self::Ref => Some(AbiParam::new(types::I64)),
        }
    }
}

/// A nullable primitive is a reference: `Int?` has to represent `null`, so it boxes, exactly as it
/// does on the JVM and as the C runtime already expects.
pub(super) fn carrier(ty: Ty) -> Carrier {
    match ty {
        Ty::Unit => Carrier::Void,
        Ty::Boolean => Carrier::Scalar(types::I8, false),
        Ty::Byte => Carrier::Scalar(types::I8, true),
        Ty::Short => Carrier::Scalar(types::I16, true),
        Ty::Char => Carrier::Scalar(types::I16, false),
        Ty::Int => Carrier::Scalar(types::I32, true),
        Ty::Long => Carrier::Scalar(types::I64, true),
        Ty::Float => Carrier::Scalar(types::F32, true),
        Ty::Double => Carrier::Scalar(types::F64, true),
        // Kotlin's unsigned integers are value classes, and common lowering erases each to the
        // signed machine integer it wraps. The carrier preserves signedness for widening,
        // comparison, and division while keeping the same physical bits.
        Ty::UByte => Carrier::Scalar(types::I8, false),
        Ty::UShort => Carrier::Scalar(types::I16, false),
        Ty::UInt => Carrier::Scalar(types::I32, false),
        Ty::ULong => Carrier::Scalar(types::I64, false),
        _ => Carrier::Ref,
    }
}

/// The runtime suffix for operations over an unboxed scalar.
pub(super) fn scalar_suffix(ty: Ty) -> Option<&'static str> {
    Some(match ty {
        Ty::Boolean => "boolean",
        Ty::Byte => "byte",
        Ty::Short => "short",
        Ty::Char => "char",
        Ty::Int => "int",
        Ty::Long => "long",
        Ty::Float => "float",
        Ty::Double => "double",
        _ => return None,
    })
}

/// The runtime suffix for a scalar's boxed representation.
pub(super) fn box_suffix(ty: Ty) -> Option<&'static str> {
    Some(match ty {
        Ty::Boolean => "boolean",
        Ty::Byte => "byte",
        Ty::Short => "short",
        Ty::Char => "char",
        Ty::Int => "int",
        Ty::Long => "long",
        Ty::Float => "float",
        Ty::Double => "double",
        Ty::UByte => "ubyte",
        Ty::UShort => "ushort",
        Ty::UInt => "uint",
        Ty::ULong => "ulong",
        _ => return None,
    })
}
