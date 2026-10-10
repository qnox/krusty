//! The checked type of a value expression, as common lowering hands it to a backend.
//!
//! Lowering records the checker's type for each FIR expression it lowers in
//! [`IrFile::logical_types`]. A node lowering synthesizes on its own (a constant chunk of a string
//! template, the zero a relational comparison is tested against, a coercion it inserts) either has
//! its type recorded there by its producer, or names that type in its own operands: a constant
//! carries its type's identity, a type operation its target, an intrinsic call its declared
//! result, and a comparison or equality is a `Boolean`. This is that contract in one place, so a
//! backend reads the checked type and chooses only the carrier. Nothing here looks at a child to
//! work out what its parent yields; a node whose type depends on its children has its type
//! recorded by its producer or has none.
//!
//! A `PrimitiveBinOp` that does not answer a `Boolean` is such a node. Its result is the result of
//! the operator the checker selected (`Int.plus(Long): Long`, `Char.plus(Int): Char`,
//! `Char.minus(Char): Int`), not a rule over its operands' types, so every producer records it
//! through [`IrFile::add_arithmetic`], and [`IrFile::unrecorded_arithmetic_result`] proves none was
//! missed before a backend sees the file. A backend picks only the instructions: which operand
//! widening to emit, and whether to narrow the answer back to the recorded type.

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
            _ => return None,
        })
    }
}

impl IrFile {
    /// An arithmetic, bitwise or shift `PrimitiveBinOp` whose value has type `result`: the declared
    /// result of the operator its producer selected.
    pub fn add_arithmetic(&mut self, op: IrBinOp, lhs: ExprId, rhs: ExprId, result: Ty) -> ExprId {
        debug_assert!(
            !op.yields_boolean(),
            "{op:?} answers a Boolean; it has no selected result type to record"
        );
        let id = self.add_expr(IrExpr::PrimitiveBinOp { op, lhs, rhs });
        self.logical_types.insert(id, result);
        id
    }

    /// The first arithmetic `PrimitiveBinOp` whose result type no producer recorded, if any.
    pub fn unrecorded_arithmetic_result(&self) -> Option<ExprId> {
        self.exprs.iter().enumerate().find_map(|(id, expression)| {
            let id = ExprId::try_from(id).expect("too many expressions");
            matches!(expression, IrExpr::PrimitiveBinOp { op, .. } if !op.yields_boolean())
                .then_some(id)
                .filter(|id| !self.logical_types.contains_key(id))
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
