//! The checked type of a value expression, as common lowering hands it to a backend.
//!
//! Lowering records the checker's type for each FIR expression it lowers in
//! [`IrFile::logical_types`]. A node lowering synthesizes on its own (a constant chunk of a string
//! template, the zero a relational comparison is tested against, a coercion it inserts) either has
//! its type recorded there by its producer, or names that type in its own operands: a constant
//! carries its type's identity, the `Unit` singleton is `Unit`, a type operation its target, an
//! intrinsic call its declared result, and a comparison or equality is a `Boolean`. This is that
//! contract in one place, so a backend reads the checked type and chooses only the carrier.
//! Nothing here looks at a child to work out what its parent yields; a node whose type depends on
//! its children has its type recorded by its producer or has none.

use super::*;

impl IrFile {
    /// The checked type of the value `id` yields, if lowering recorded one.
    pub fn checked_type(&self, id: ExprId) -> Option<Ty> {
        if let Some(ty) = self.logical_types.get(&id) {
            return Some(*ty);
        }
        Some(match self.expr(id) {
            IrExpr::Const(constant) => constant.checked_type(),
            IrExpr::TypeOp {
                op, type_operand, ..
            } => match op {
                IrTypeOp::InstanceOf | IrTypeOp::NotInstanceOf => Ty::Boolean,
                IrTypeOp::SafeCast => Ty::nullable(*type_operand),
                IrTypeOp::Cast | IrTypeOp::CastNonNull | IrTypeOp::ImplicitCoercion => {
                    *type_operand
                }
            },
            IrExpr::Call {
                callee: Callee::Intrinsic { ret, .. },
                ..
            } => *ret,
            IrExpr::PrimitiveBinOp { op, .. } if op.yields_boolean() => Ty::Boolean,
            IrExpr::Equality { .. } => Ty::Boolean,
            IrExpr::PrimitiveNeg { ty, .. } => *ty,
            IrExpr::StringConcat(_) => Ty::String,
            IrExpr::UnitInstance => Ty::Unit,
            _ => return None,
        })
    }
}

impl IrConst {
    /// The checked type a constant names: its own identity, or `Nothing?` for `null`.
    pub fn checked_type(&self) -> Ty {
        match self {
            IrConst::Boolean(_) => Ty::Boolean,
            IrConst::Byte(_) => Ty::Byte,
            IrConst::Short(_) => Ty::Short,
            IrConst::Int(_) => Ty::Int,
            IrConst::Long(_) => Ty::Long,
            IrConst::Float(_) => Ty::Float,
            IrConst::Double(_) => Ty::Double,
            IrConst::Char(_) => Ty::Char,
            IrConst::String(_) => Ty::String,
            IrConst::UByte(_) => Ty::UByte,
            IrConst::UShort(_) => Ty::UShort,
            IrConst::UInt(_) => Ty::UInt,
            IrConst::ULong(_) => Ty::ULong,
            IrConst::Null => Ty::Null,
        }
    }
}
