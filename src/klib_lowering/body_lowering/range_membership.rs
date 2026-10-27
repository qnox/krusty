//! `value in start..end` over a builtin range literal, lowered as checked FIR lowers the source
//! membership: a comparison with the literal's bounds, never the range or its `contains`.

use super::super::primitive_calls::CommonPrimitive;
use super::super::range_membership::RangeLiteralMembership;
use super::calls::arguments;
use super::{mismatch, unsupported, BodyLowering, Lowered};
use crate::ir::{ExprId, IrCheckedOperation, IrExpr};
use crate::libraries::KlibDeclarationSignature;
use crate::metadata::klib_ir::KlibIrSignature;
use crate::types::Ty;

impl BodyLowering<'_> {
    /// The comparison a membership in a range literal is, when its range operator is a builtin
    /// scalar's range construction and its `contains` is the constructed range's own membership
    /// operator over that scalar. `None` keeps the call an ordinary call of `contains`: when a
    /// checked source edge or retained export needs that declaration, or when the linker keeps
    /// memberships as calls. Otherwise the membership's declaration edge was never prepared, so a
    /// form this does not prove declines the body rather than calling an unprepared declaration.
    pub(super) fn range_literal_membership(
        &mut self,
        membership: RangeLiteralMembership<'_>,
        ty: Ty,
        negated: bool,
    ) -> Lowered<Option<ExprId>> {
        let (KlibIrSignature::Public(range), KlibIrSignature::Public(contains)) = (
            &membership.range.symbol.signature,
            &membership.membership.symbol.signature,
        ) else {
            return Err(mismatch(
                "a range literal membership names a non-public declaration",
            ));
        };
        if self.linker.requires_declaration(contains)? {
            return Ok(None);
        }
        let Some(header) = self.linker.range_membership_header(contains)? else {
            return Ok(None);
        };
        let Some(construction) = self
            .linker
            .common_primitive(&KlibDeclarationSignature::Public(range.clone()))?
        else {
            return unsupported("a membership in another range than a builtin range construction");
        };
        let CommonPrimitive::RangeConstruction(operation) = construction.operation else {
            return unsupported("a membership in another range than a builtin range construction");
        };
        let element = construction.receiver;
        // Mixed bounds and unsigned ranges compare through a widened or provider-selected
        // operation that this form does not record.
        if construction.params != [element] || !matches!(element, Ty::Int | Ty::Long | Ty::Char) {
            return unsupported("a membership in a range of mixed or unsigned bounds");
        }
        if header.receiver != Some(construction.ret)
            || header.params != [element]
            || header.ret != Ty::Boolean
        {
            return Err(mismatch(
                "a range literal's `contains` is not the constructed range's membership operator",
            ));
        }
        if ty != Ty::Boolean {
            return Err(mismatch("a range membership is not typed `Boolean`"));
        }
        let &[Some(start), Some(end)] = arguments(membership.range).as_slice() else {
            return Err(mismatch(
                "a range operator is called with another shape than its declaration's",
            ));
        };
        let start = self.operand(start, element)?;
        let end = self.operand(end, element)?;
        let value = self.operand(membership.value, element)?;
        Ok(Some(self.ir.add_expr(IrExpr::Checked(Box::new(
            IrCheckedOperation::RangeContains {
                operation,
                value,
                start,
                end,
                negated,
                counter: element,
                unsigned_compare: None,
            },
        )))))
    }
}
