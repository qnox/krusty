//! Which initializer stores write only what freshly allocated storage already holds.

use super::{ExprId, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::types::Ty;

impl IrFile {
    /// Is `expression` a declaration's initializer store that writes only what a freshly allocated
    /// object's storage already holds, and so must not be emitted?
    ///
    /// `var x = 0` in a class body stores nothing: kotlinc omits an initializer that writes the
    /// value fresh storage already holds (`null`, a zero of any width, `false`). The omission is
    /// observable, not an optimization: a base-class constructor that dispatches to an override runs
    /// BEFORE the subclass's initializers, so a value it wrote through that override survives
    /// exactly because the declaration's own store was never emitted. A later `init { x = 0 }` is a
    /// different statement with a different meaning, which is why the store's exact identity comes
    /// from `property_initializer_stores` rather than from its shape.
    ///
    /// Every target krusty emits for clears an object's storage when it allocates, so this is one
    /// rule for every backend.
    pub fn is_elided_initializer_store(&self, expression: ExprId) -> bool {
        self.property_initializer_stores.contains(&expression)
            && matches!(self.expr(expression), IrExpr::SetField { class, index, value, .. }
                if self.is_storage_default(self.field_storage(*class, *index), *value))
    }

    /// Is `expression` the value freshly allocated `storage` already holds? A reference slot holds
    /// only `null`: `val boxed: Int? = 0` stores a boxed zero, which is not the default. A scalar
    /// slot holds a zero of its width (signed or unsigned, whose carrier is the same) or `false`.
    pub fn is_storage_default(&self, storage: Ty, expression: ExprId) -> bool {
        if storage.is_reference() {
            return self.is_null_constant(expression);
        }
        self.is_scalar_zero(expression)
    }

    fn is_null_constant(&self, expression: ExprId) -> bool {
        match self.expr(expression) {
            IrExpr::Const(IrConst::Null) => true,
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => self.is_null_constant(*arg),
            _ => false,
        }
    }

    fn is_scalar_zero(&self, expression: ExprId) -> bool {
        match self.expr(expression) {
            IrExpr::Const(IrConst::Boolean(false))
            | IrExpr::Const(IrConst::Byte(0))
            | IrExpr::Const(IrConst::Short(0))
            | IrExpr::Const(IrConst::Int(0))
            | IrExpr::Const(IrConst::Long(0))
            | IrExpr::Const(IrConst::Char(0))
            | IrExpr::Const(IrConst::UByte(0))
            | IrExpr::Const(IrConst::UShort(0))
            | IrExpr::Const(IrConst::UInt(0))
            | IrExpr::Const(IrConst::ULong(0)) => true,
            IrExpr::Const(IrConst::Float(value)) => value.to_bits() == 0,
            IrExpr::Const(IrConst::Double(value)) => value.to_bits() == 0,
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg,
                ..
            } => self.is_scalar_zero(*arg),
            _ => false,
        }
    }

    fn field_storage(&self, class: super::ClassId, index: u32) -> Ty {
        self.classes[class as usize].fields[index as usize].ty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A class whose field 0 is a scalar `Int` and field 1 a nullable `Int?` reference.
    fn file_with_scalar_and_reference_fields() -> IrFile {
        let mut ir = IrFile::default();
        let mut holder = crate::plugins::synthetic_class("fixture/Holder");
        holder
            .fields
            .push(crate::ir::IrField::new("scalar".to_string(), Ty::Int));
        holder.fields.push(crate::ir::IrField::new(
            "boxed".to_string(),
            Ty::nullable(Ty::Int),
        ));
        ir.add_class(holder);
        ir
    }

    /// A declaration's store of what fresh storage already holds is elided; the same store written
    /// as a later statement is not, and neither is a declaration's store of anything else.
    ///
    /// The identity decides, not the shape: `var x = 0` followed by `init { x = 0 }` is two stores
    /// of the same field and value, and only the first is the declaration's.
    #[test]
    fn only_a_declarations_store_of_the_storage_default_is_elided() {
        let mut ir = file_with_scalar_and_reference_fields();
        let receiver = ir.add_expr(IrExpr::GetValue(0));
        let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let one = ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let store = |ir: &mut IrFile, index, value| {
            ir.add_expr(IrExpr::SetField {
                receiver,
                class: 0,
                index,
                value,
            })
        };
        let declaration = store(&mut ir, 0, zero);
        let later = store(&mut ir, 0, zero);
        let declared_one = store(&mut ir, 0, one);
        let declared_boxed_zero = store(&mut ir, 1, zero);
        for recorded in [declaration, declared_one, declared_boxed_zero] {
            ir.property_initializer_stores.insert(recorded);
        }

        assert_eq!(
            [declaration, later, declared_one, declared_boxed_zero]
                .map(|store| ir.is_elided_initializer_store(store)),
            [true, false, false, false]
        );
    }

    /// Fresh scalar storage holds all-zero bits: `false` and a zero of every width, signed or
    /// unsigned. A floating zero counts only with its sign bit clear, since `-0.0` is not those
    /// bits. Fresh reference storage holds only `null`, so a boxed zero or `false` is a real store.
    #[test]
    fn the_storage_default_follows_the_storage() {
        let mut ir = IrFile::default();
        let zeros = [
            IrConst::Boolean(false),
            IrConst::Byte(0),
            IrConst::Short(0),
            IrConst::Int(0),
            IrConst::Long(0),
            IrConst::Char(0),
            IrConst::UByte(0),
            IrConst::UShort(0),
            IrConst::UInt(0),
            IrConst::ULong(0),
            IrConst::Float(0.0),
            IrConst::Double(0.0),
        ];
        let non_zeros = [
            IrConst::Null,
            IrConst::Boolean(true),
            IrConst::Int(1),
            IrConst::UInt(1),
            IrConst::Float(-0.0),
            IrConst::Double(-0.0),
        ];
        for (constants, expected) in [(zeros.as_slice(), true), (non_zeros.as_slice(), false)] {
            for constant in constants {
                let expression = ir.add_expr(IrExpr::Const(constant.clone()));
                assert_eq!(
                    ir.is_storage_default(Ty::Int, expression),
                    expected,
                    "{constant:?} in scalar storage"
                );
                assert_eq!(
                    ir.is_storage_default(Ty::nullable(Ty::Int), expression),
                    *constant == IrConst::Null,
                    "{constant:?} in reference storage"
                );
            }
        }
    }
}
