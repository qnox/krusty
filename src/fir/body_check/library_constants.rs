//! A library constant's value as checked FIR carries it: the constant's declared type decides how
//! its stored integer reads (a `Boolean`, a `Char`, or an unsigned value).

use crate::fir::FirConstant;
use crate::types::Ty;

pub(super) fn checked_constant_value(constant: &crate::libraries::LibraryConst) -> FirConstant {
    match &constant.value {
        crate::libraries::LibConst::Int(value) => match constant.ty.non_null() {
            Ty::Boolean => FirConstant::Boolean(*value != 0),
            Ty::Char => FirConstant::Char(*value as u16),
            Ty::UInt | Ty::UByte | Ty::UShort => FirConstant::UInt(i64::from(*value as u32)),
            _ => FirConstant::Int(i64::from(*value)),
        },
        crate::libraries::LibConst::Long(value) => match constant.ty.non_null() {
            Ty::ULong => FirConstant::ULong(*value),
            _ => FirConstant::Long(*value),
        },
        crate::libraries::LibConst::Float(value) => FirConstant::Float(*value),
        crate::libraries::LibConst::Double(value) => FirConstant::Double(*value),
        crate::libraries::LibConst::Str(value) => FirConstant::String(value.clone()),
    }
}
