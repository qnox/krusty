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
    /// The declared semantic type of every local value slot in `root`'s value namespace.
    ///
    /// A nested lambda's captures still read this namespace, but its inline body has the lambda
    /// function's own slots. [`value_namespace_expressions`] observes that boundary, so two
    /// unrelated slot zeroes never enter the same inventory.
    pub(crate) fn declared_value_slot_types(
        &self,
        root: ExprId,
    ) -> crate::fx_hash::HashMap<u32, Ty> {
        let mut declarations = crate::fx_hash::HashMap::default();
        let mut declare = |slot, ty| {
            let previous = declarations.insert(slot, ty);
            debug_assert!(
                previous.is_none_or(|previous| previous == ty),
                "common-IR value slot {slot} has two semantic declaration types"
            );
        };
        for expression in super::value_namespace_expressions(self, root) {
            match self.expr(expression) {
                IrExpr::Variable { index, ty, .. } => declare(*index, *ty),
                IrExpr::Try { catches, .. } => {
                    for catch in catches {
                        declare(catch.var, catch.ty);
                    }
                }
                IrExpr::Checked(checked_op)
                    if let IrCheckedOperation::RangeLoop {
                        variable, counter, ..
                    } = &**checked_op =>
                {
                    declare(*variable, *counter)
                }
                _ => {}
            }
        }
        declarations
    }

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
                callee: call_callee,
                ..
            } if let Callee::Intrinsic { ret, .. } = &**call_callee => *ret,
            // These property-read nodes carry the checker-selected logical result. A later
            // backend decides whether the physical realization is a field or an accessor.
            IrExpr::PropertyRead { ty, .. } => *ty,
            IrExpr::Checked(checked_op)
                if let IrCheckedOperation::ExternalPropertyRead { result, .. } = &**checked_op =>
            {
                *result
            }
            IrExpr::PrimitiveBinOp { op, .. } if op.yields_boolean() => Ty::Boolean,
            IrExpr::Equality { .. } => Ty::Boolean,
            IrExpr::PrimitiveNeg { ty, .. } | IrExpr::AbsentArgument { ty } => *ty,
            IrExpr::StringConcat(_) => Ty::String,
            IrExpr::UnitInstance => Ty::Unit,
            // A construction, a singleton and an enum constant name their class; an array
            // construction and a callable reference name their whole type.
            IrExpr::New { internal, .. } => Ty::obj_name(*internal),
            IrExpr::SingletonValue { classifier }
            | IrExpr::EnumEntry { classifier, .. }
            | IrExpr::EnumValueOf { classifier, .. } => Ty::obj_name(*classifier),
            // An enclosing-instance edge names the outer classifier it reaches.
            IrExpr::EnclosingInstance { outer, .. } => Ty::obj_name(*outer),
            IrExpr::EnumValues { classifier } => {
                Ty::obj_args("kotlin/Array", &[Ty::obj_name(*classifier)])
            }
            IrExpr::EnumEntries { classifier } => {
                Ty::obj_args("kotlin/enums/EnumEntries", &[Ty::obj_name(*classifier)])
            }
            IrExpr::EnumSwitchOrdinal { .. } | IrExpr::EnumSwitchKey { .. } => Ty::Int,
            IrExpr::NewArray { array_type, .. } | IrExpr::Vararg { array_type, .. } => *array_type,
            IrExpr::CallableReference(reference) => reference.function_type,
            IrExpr::Checked(checked_op)
                if let IrCheckedOperation::RangeConstruction { result, .. } = &**checked_op =>
            {
                *result
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_inventory_includes_every_common_ir_slot_declaration() {
        let mut file = IrFile::default();
        let unit = file.add_expr(IrExpr::UnitInstance);
        let local = file.add_expr(IrExpr::Variable {
            index: 3,
            ty: Ty::String,
            init: None,
            named: true,
        });
        let guarded = file.add_expr(IrExpr::Try {
            body: unit,
            catches: vec![IrCatch {
                var: 5,
                binding: None,
                ty: Ty::obj("sample/Failure"),
                body: unit,
                line: None,
            }],
            finally: None,
            result: Ty::Unit,
        });
        let start = file.add_expr(IrExpr::Const(IrConst::Long(0)));
        let end = file.add_expr(IrExpr::Const(IrConst::Long(1)));
        let loop_ = file.add_expr(IrExpr::Checked(Box::new(IrCheckedOperation::RangeLoop {
            variable: 8,
            variable_name: None,
            counter: Ty::Long,
            source: IrProgressionSource::Literal {
                operation: crate::fir::FirRangeOperation::Through,
                start,
                end,
            },
            unsigned_compare: None,
            body: unit,
            label: "loop".to_string(),
            with_index: None,
        })));
        let root = file.add_expr(IrExpr::Block {
            stmts: vec![local, guarded, loop_],
            value: None,
        });

        assert_eq!(
            file.declared_value_slot_types(root),
            crate::fx_hash::HashMap::from_iter([
                (3, Ty::String),
                (5, Ty::obj("sample/Failure")),
                (8, Ty::Long),
            ])
        );
    }

    #[test]
    fn enum_switch_nodes_name_their_checked_int_type() {
        let mut file = IrFile::default();
        let subject = file.add_expr(IrExpr::Const(IrConst::Int(0)));
        let classifier = crate::types::type_name("sample/Choice");
        let ordinal = file.add_expr(IrExpr::EnumSwitchOrdinal {
            classifier,
            subject,
            registrations: Box::new([]),
        });
        let key = file.add_expr(IrExpr::EnumSwitchKey {
            classifier,
            name: "First".into(),
        });

        assert_eq!(file.checked_type(ordinal), Some(Ty::Int));
        assert_eq!(file.checked_type(key), Some(Ty::Int));
    }
}
