//! Checked publication of resolver-planned runtime type tests.

use crate::ast::{ExprId, TypeRef};
use crate::fir::{FirExprKind, FirTypeOperation, FirTypeTestPlan};
use crate::resolve::ExprLowering;

use super::{BodyCheckFailure, BodyCheckFailureKind, BodyFirChecker};

impl BodyFirChecker<'_> {
    pub(super) fn type_test(
        &mut self,
        expression: ExprId,
        operand: ExprId,
        ty: &TypeRef,
        negated: bool,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let (runtime_target, same_erased_upper_bound) = match self.info.expr_lowers.get(&expression)
        {
            Some(ExprLowering::RuntimeTypeTest {
                target,
                same_erased_upper_bound,
            }) => (*target, *same_erased_upper_bound),
            _ => panic!("checked type test must publish its runtime plan"),
        };
        self.info.resolved_type(ty).ok_or_else(|| {
            self.failure(Some(ty.span), BodyCheckFailureKind::UnresolvedTypeSyntax)
        })?;
        let plan = FirTypeTestPlan {
            same_erased_upper_bound,
        };
        Ok(FirExprKind::TypeOperation {
            operation: if negated {
                FirTypeOperation::NotIs(plan)
            } else {
                FirTypeOperation::Is(plan)
            },
            operand: self.expression(operand)?,
            target: self.resolved_type(ty.span, runtime_target)?,
        })
    }
}
