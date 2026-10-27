//! kotlinc's `RangeContainsLowering` over an `IndicesHandler` header: `value in receiver.indices`
//! compares the value with 0 and with the receiver's size instead of building the range and
//! calling its `contains`.

use super::*;
use crate::fir::FirExprKind;
use crate::libraries::CompilerIntrinsic;
use crate::resolve::ResolvedCall;

impl BodyFirChecker<'_> {
    /// The membership test the `in` or `!in` call `expression` stands for, when its receiver is an
    /// `indices` read, the selected `contains` is the range's own member or a stdlib closed-range
    /// extension (`matchStdlibExtensionContainsCall`), and the value is of a primitive numeric
    /// type. The comparison is in the widest numeric type of the `Int` bounds and the value
    /// (`computeComparisonClass`). A `!in` keeps its negation of this test, as kotlinc keeps its
    /// `not()` call.
    pub(super) fn indices_membership(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        arguments: &[ExprId],
    ) -> Result<Option<FirExprKind>, BodyCheckFailure> {
        if !self.file.operator_token_spans.contains_key(&expression.0) {
            return Ok(None);
        }
        let [value] = *arguments else {
            return Ok(None);
        };
        if self.info.indices_size(receiver).is_none() {
            return Ok(None);
        }
        let range_contains = match self.info.resolved_calls.get(&expression) {
            Some(ResolvedCall::Member(_)) => true,
            Some(ResolvedCall::Extension(extension)) => {
                extension.callable.compiler_intrinsic
                    == Some(CompilerIntrinsic::ClosedRangeContains)
            }
            _ => false,
        };
        // `IntRange.contains(Int?)` and any value not of a primitive numeric class are called.
        let value_ty = self.info.semantic_ty(value);
        let Some(comparison) = (range_contains && value_ty.is_numeric())
            .then(|| {
                crate::resolve::operator_calls::range_comparison_type(Ty::Int, Ty::Int, value_ty)
            })
            .flatten()
        else {
            return Ok(None);
        };
        let span = self
            .file
            .expr_span(expression)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let comparison = self.resolved_type(span, comparison)?;
        let checked_receiver = self.expression(receiver)?;
        let Some(indices) = self.checked_indices(receiver, checked_receiver)? else {
            unreachable!("the receiver has an `indices` size plan");
        };
        Ok(Some(FirExprKind::IndicesContainment {
            value: self.direct_range_operand(value, comparison)?,
            indices: Box::new(indices),
            comparison,
        }))
    }
}
