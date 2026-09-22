//! JVM emission for reference-to-reference implicit coercions.

use crate::ir::{IrConst, IrExpr};
use crate::jvm::classfile::{ClassWriter, CodeBuilder};
use crate::jvm::names::{instanceof_internal_name, type_descriptor};
use crate::types::Ty;

/// Narrow one already-emitted reference operand when its physical target requires a JVM cast.
///
/// A direct null literal has no runtime class to narrow: `aconst_null` satisfies every reference
/// target, and kotlinc stores it without an otherwise-redundant `checkcast`. Non-null operands keep
/// the narrowing required by their distinct physical descriptors.
pub(super) fn emit(
    operand: &IrExpr,
    source: Ty,
    target: Ty,
    cw: &mut ClassWriter,
    code: &mut CodeBuilder,
) {
    if !source.is_reference()
        || !target.is_reference()
        || type_descriptor(source) == type_descriptor(target)
        || matches!(operand, IrExpr::Const(IrConst::Null))
    {
        return;
    }

    let internal = instanceof_internal_name(target);
    if internal != "java/lang/Object" {
        code.checkcast(cw.class_ref(&internal));
    }
}
