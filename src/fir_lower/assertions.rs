//! Lowering for the checker-selected Kotlin `assert` operation.
//!
//! Unlike an ordinary call, an assertion's operands are conditionally evaluated. Keep them as
//! direct children of the common-IR intrinsic instead of passing through ordinary call-operand
//! normalization, which is allowed to spill and eagerly execute operands before the call.

use crate::fir::{FirCallArgument, ResolvedTy};
use crate::ir::{Callee, ExprId, IrExpr, IrIntrinsic};
use crate::types::{AssertionMode, Ty};

use super::{BodyLowering, FirLoweringFailure};

impl BodyLowering<'_> {
    pub(super) fn assertion_call(
        &mut self,
        mode: AssertionMode,
        dispatch_receiver: Option<ExprId>,
        extension_receiver: Option<ExprId>,
        arguments: &[FirCallArgument],
        parameters: &[ResolvedTy],
        result: ResolvedTy,
    ) -> Result<ExprId, FirLoweringFailure> {
        if dispatch_receiver.is_some()
            || extension_receiver.is_some()
            || result.get() != Ty::Unit
            || !matches!(parameters, [condition] if condition.get() == Ty::Boolean)
                && !matches!(parameters, [condition, _] if condition.get() == Ty::Boolean)
        {
            return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
        }

        // Always-disabled assertions discard the entire operand graph. In particular, do not even
        // lower the children: lowering a condition into an enclosing statement block would turn
        // semantic elision into eager execution.
        let mut operands = if mode == AssertionMode::AlwaysDisabled {
            Vec::new()
        } else {
            vec![None; parameters.len()]
        };
        if mode != AssertionMode::AlwaysDisabled {
            for argument in arguments {
                let FirCallArgument::Expression {
                    parameter,
                    value,
                    conversion,
                } = argument
                else {
                    return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
                };
                let slot = operands.get_mut(*parameter as usize).ok_or(
                    FirLoweringFailure::MissingExternalParameter {
                        parameter: *parameter,
                    },
                )?;
                if slot.is_some() {
                    return Err(FirLoweringFailure::UnsupportedIntrinsicCall);
                }
                *slot = Some(self.expression_with_conversion(*value, *conversion)?);
            }
        }
        let args = operands
            .into_iter()
            .collect::<Option<Vec<ExprId>>>()
            .ok_or(FirLoweringFailure::UnsupportedIntrinsicCall)?;
        Ok(self.ir.add_expr(IrExpr::Call {
            callee: Callee::Intrinsic {
                operation: IrIntrinsic::Assert { mode },
                ret: Ty::Unit,
            },
            dispatch_receiver: None,
            args,
        }))
    }
}
