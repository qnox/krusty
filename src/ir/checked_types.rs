//! The checked type of a value expression, as common lowering hands it to a backend.
//!
//! Lowering records the checker's type for each FIR expression it lowers in
//! [`IrFile::logical_types`]. A node lowering synthesizes on its own (a constant chunk of a string
//! template, the zero a relational comparison is tested against, a coercion it inserts) either has
//! its type recorded there by its producer, or names that type in its own operands: a constant
//! carries its type's identity, the `Unit` singleton is `Unit`, a type operation its target, an
//! intrinsic call its declared result, a construction or singleton its class, an array or callable
//! reference its whole type, and a comparison or equality is a `Boolean`. This is that contract in
//! one place, so a backend reads the checked type and chooses only the carrier. Nothing here looks
//! at a child to work out what its parent yields; a node whose type depends on its children has its
//! type recorded by its producer or has none.
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
        match self.logical_types.get(&id) {
            Some(ty) => Some(*ty),
            None => self.named_type(id),
        }
    }

    /// The type node `id` names in itself, whatever lowering recorded beside it: what a target
    /// produces when it evaluates the node. It is the checked type unless a later pass recorded a
    /// more specific one, as an inline call's substitution of a type parameter does.
    pub fn named_type(&self, id: ExprId) -> Option<Ty> {
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
            // A construction, a singleton and an enum constant name their class; an array
            // construction and a callable reference name their whole type.
            IrExpr::New { internal, .. } => Ty::Obj(*internal, &[]),
            IrExpr::SingletonValue { classifier }
            | IrExpr::EnumEntry { classifier, .. }
            | IrExpr::EnumValueOf { classifier, .. } => Ty::Obj(*classifier, &[]),
            // An enclosing-instance edge names the outer classifier it reaches.
            IrExpr::EnclosingInstance { outer, .. } => Ty::Obj(*outer, &[]),
            IrExpr::EnumValues { classifier } => {
                Ty::obj_args("kotlin/Array", &[Ty::Obj(*classifier, &[])])
            }
            IrExpr::EnumEntries { classifier } => {
                Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::Obj(*classifier, &[])])
            }
            IrExpr::NewArray { array_type, .. } | IrExpr::Vararg { array_type, .. } => *array_type,
            IrExpr::CallableReference(reference) => reference.function_type,
            IrExpr::Checked(IrCheckedOperation::RangeConstruction { result, .. }) => *result,
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
