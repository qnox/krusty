//! Integer-literal leaves in the compact signature graph.

use super::*;
use crate::integer_constant::IntegerConstant;

impl SignatureConstraintExtractor {
    pub(super) fn integer_literal(&mut self, value: i64) -> SigExprId {
        match i32::try_from(value) {
            Ok(value) => self.graph.add_expr(SigExpr::IntegerLiteral(value)),
            Err(_) => self.known(Ty::Int),
        }
    }

    pub(super) fn unsigned_integer_literal(&mut self, value: i64) -> SigExprId {
        let constant = u64::try_from(value)
            .ok()
            .map(IntegerConstant::Unsigned)
            .filter(|constant| constant.fits(Ty::UInt));
        match constant {
            Some(IntegerConstant::Unsigned(magnitude)) => self
                .graph
                .add_expr(SigExpr::UnsignedIntegerLiteral(magnitude)),
            _ => self.known(Ty::UInt),
        }
    }
}
