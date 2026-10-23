//! Class literals.
//!
//! A class literal lowers as checked FIR lowers the source literal: `C::class` and `T::class` name
//! their type, and `value::class` the value whose runtime class is requested. The literal is typed
//! as the `KClass` the KLIB serializes it at.

use super::{mismatch, BodyLowering, Lowered};
use crate::ir::{ExprId, IrExpr};
use crate::metadata::klib_ir::tree::{KlibIrExprId, KlibIrTypeId};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::{wk, Ty};

impl BodyLowering<'_> {
    /// `C::class` or `T::class` of the class or type parameter `class`, whose type is `class_type`.
    pub(super) fn class_reference(
        &mut self,
        class: &KlibIrSymbol,
        class_type: KlibIrTypeId,
        ty: Ty,
    ) -> Lowered<ExprId> {
        let classifier =
            super::semantic_type(self.arena, self.header.type_parameters(), class_type)?;
        let names = match (class.kind, &class.signature) {
            (KlibIrSymbolKind::TypeParameter, _) => classifier.ty_param_name().is_some(),
            (KlibIrSymbolKind::Class, KlibIrSignature::Public(signature)) => {
                let named = super::super::klib_types::classifier_name(signature)?;
                crate::types::builtin_semantic(named).unwrap_or(Ty::obj_name(named))
                    == classifier.non_null()
                    || classifier.non_null().kotlin_class_internal() == Some(named)
            }
            _ => false,
        };
        if !names {
            return Err(mismatch(
                "a class literal's type is not the type its symbol names",
            ));
        }
        self.class_literal(Some(classifier), None, ty)
    }

    /// `value::class`: the runtime class of `value`.
    pub(super) fn value_class_literal(&mut self, value: KlibIrExprId, ty: Ty) -> Lowered<ExprId> {
        let value = self.expression(value)?;
        self.class_literal(None, Some(value), ty)
    }

    fn class_literal(
        &mut self,
        classifier: Option<Ty>,
        value: Option<ExprId>,
        ty: Ty,
    ) -> Lowered<ExprId> {
        if ty.is_nullable() || ty.kotlin_class_internal() != Some(wk::kclass()) {
            return Err(mismatch("a class literal is not typed as a `KClass`"));
        }
        let literal = self.ir.add_expr(IrExpr::KClassLiteral {
            classifier,
            value,
            type_argument: classifier.is_some_and(|ty| ty.ty_param_name().is_some()),
        });
        Ok(self.typed(literal, ty))
    }
}
