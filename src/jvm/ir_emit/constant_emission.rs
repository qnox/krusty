//! JVM operand emission for a semantic common-IR constant.

use super::*;

pub(super) fn emit(constant: &IrConst, code: &mut CodeBuilder, cw: &mut ClassWriter) {
    match constant {
        IrConst::Boolean(value) => code.push_int(i32::from(*value), cw),
        IrConst::Int(value) => code.push_int(*value, cw),
        // Unsigned values use the signed JVM carrier of the same width.
        IrConst::UByte(value) => code.push_int(i32::from(*value as i8), cw),
        IrConst::UShort(value) => code.push_int(i32::from(*value as i16), cw),
        IrConst::UInt(value) => code.push_int(*value as i32, cw),
        IrConst::ULong(value) => code.push_long(*value as i64, cw),
        IrConst::Short(value) => code.push_int(i32::from(*value), cw),
        IrConst::Byte(value) => code.push_int(i32::from(*value), cw),
        IrConst::Char(value) => code.push_int(i32::from(*value), cw),
        IrConst::Long(value) => code.push_long(*value, cw),
        IrConst::Double(value) => code.push_double(*value, cw),
        IrConst::Float(value) => code.push_float(*value, cw),
        IrConst::String(value) => super::super::string_constant::push_string(value, code, cw),
        IrConst::Null => code.aconst_null(),
    }
}
