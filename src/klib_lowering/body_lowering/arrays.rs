//! Serialized array operands use the same packed vararg node as checked source lowering.

use super::{mismatch, semantic_type, BodyLowering, ExprId, IrExpr, Lowered, Ty};
use crate::metadata::klib_ir::tree::{KlibIrTypeId, KlibIrVarargElement};

impl BodyLowering<'_> {
    pub(super) fn vararg_array(
        &mut self,
        element_type: KlibIrTypeId,
        elements: &[KlibIrVarargElement],
        array_type: Ty,
    ) -> Lowered<ExprId> {
        let element =
            super::super::function_lowering::vararg_parameters::array_element(array_type)?;
        if semantic_type(self.arena, self.header.type_parameters(), element_type)? != element {
            return Err(mismatch(
                "a serialized vararg expression disagrees with its packed array element",
            ));
        }
        let mut values = Vec::with_capacity(elements.len());
        let mut spreads = Vec::with_capacity(elements.len());
        for value in elements {
            let (expression, spread) = match value {
                KlibIrVarargElement::Expression(expression) => (*expression, false),
                KlibIrVarargElement::Spread(expression) => (*expression, true),
            };
            values.push(self.operand(expression, if spread { array_type } else { element })?);
            spreads.push(spread);
        }
        Ok(self.ir.add_expr(IrExpr::Vararg {
            array_type,
            elements: values,
            spreads,
        }))
    }
}
