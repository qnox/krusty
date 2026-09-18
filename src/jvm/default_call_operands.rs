//! JVM-owned operands synthesized for one checked default call.
//!
//! Common IR retains supplied arguments and omitted semantic parameter ordinals. JVM realization
//! materializes placeholders, mask words, and the marker. This side table keeps that physical plan
//! beside the backend, keyed by the stable call expression, so emission never guesses provenance
//! from a zero/null constant and common IR never carries JVM ABI facts.

use std::collections::HashMap;

use crate::ir::ExprId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DefaultOperandOrigin {
    Supplied,
    Synthesized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DefaultCallOperand {
    pub(super) expression: ExprId,
    pub(super) origin: DefaultOperandOrigin,
    /// The first such operand begins the JVM-only `mask..., marker` suffix. Suspend lowering inserts
    /// the continuation immediately before it; carrying the boundary avoids rediscovering it from
    /// constant values, types, or descriptor spelling.
    abi_suffix: bool,
}

impl DefaultCallOperand {
    pub(super) fn supplied(expression: ExprId) -> Self {
        Self {
            expression,
            origin: DefaultOperandOrigin::Supplied,
            abi_suffix: false,
        }
    }

    pub(super) fn synthesized(expression: ExprId) -> Self {
        Self {
            expression,
            origin: DefaultOperandOrigin::Synthesized,
            abi_suffix: false,
        }
    }

    pub(super) fn synthesized_abi(expression: ExprId) -> Self {
        Self {
            expression,
            origin: DefaultOperandOrigin::Synthesized,
            abi_suffix: true,
        }
    }
}

#[derive(Default)]
pub(crate) struct DefaultCallOperands {
    entries: HashMap<ExprId, Vec<DefaultCallOperand>>,
}

impl DefaultCallOperands {
    pub(super) fn record(&mut self, call: ExprId, operands: Vec<DefaultCallOperand>) {
        let previous = self.entries.insert(call, operands);
        debug_assert!(
            previous.is_none(),
            "a default call is realized exactly once"
        );
    }

    /// The physical plan only when it still exactly matches the call's realized operand vector.
    /// A later backend rewrite that changes the vector must update this table rather than letting
    /// emission apply stale provenance by position.
    pub(super) fn matching(
        &self,
        call: ExprId,
        operands: &[ExprId],
    ) -> Option<&[DefaultCallOperand]> {
        self.entries.get(&call).map(Vec::as_slice).filter(|plan| {
            plan.len() == operands.len()
                && plan
                    .iter()
                    .zip(operands)
                    .all(|(planned, actual)| planned.expression == *actual)
        })
    }

    pub(super) fn contains(&self, call: ExprId) -> bool {
        self.entries.contains_key(&call)
    }

    /// Insert the CPS continuation at the exact boundary recorded when the default ABI operands
    /// were materialized. `None` means this is not a default call; a recorded plan without a suffix
    /// is an invalid backend state and fails closed at its caller.
    pub(super) fn insert_continuation(
        &mut self,
        call: ExprId,
        continuation: ExprId,
    ) -> Result<Option<usize>, ()> {
        let Some(plan) = self.entries.get_mut(&call) else {
            return Ok(None);
        };
        let index = plan
            .iter()
            .position(|operand| operand.abi_suffix)
            .ok_or(())?;
        plan.insert(index, DefaultCallOperand::supplied(continuation));
        Ok(Some(index))
    }

    /// A suspend transform may bind the already-planned operands to fresh locals. Preserve their
    /// supplied/synthesized provenance by position while replacing the exact expression identities.
    pub(super) fn replace_operands(&mut self, call: ExprId, operands: &[ExprId]) -> bool {
        let Some(plan) = self.entries.get_mut(&call) else {
            return true;
        };
        if plan.len() != operands.len() {
            return false;
        }
        for (planned, replacement) in plan.iter_mut().zip(operands) {
            planned.expression = *replacement;
        }
        true
    }

    /// Reattach every plan to the final operands after a backend transform has rewritten call
    /// children in place while preserving their order. Provenance and the ABI-suffix boundary are
    /// positional contracts; a changed arity or a non-call owner is an invalid backend state.
    pub(super) fn synchronize(&mut self, ir: &crate::ir::IrFile) -> bool {
        for (&call, plan) in &mut self.entries {
            let Some(crate::ir::IrExpr::Call { args, .. }) = ir.exprs.get(call as usize) else {
                return false;
            };
            if plan.len() != args.len() {
                return false;
            }
            for (planned, actual) in plan.iter_mut().zip(args) {
                planned.expression = *actual;
            }
        }
        true
    }

    /// Clone one per-call plan after common-IR DAG unsharing. The cloned call owns cloned operand
    /// identities but exactly the same provenance and ABI boundary.
    pub(super) fn clone_call(
        &mut self,
        source: ExprId,
        target: ExprId,
        operands: &[ExprId],
    ) -> bool {
        let Some(source_plan) = self.entries.get(&source).cloned() else {
            return true;
        };
        if source_plan.len() != operands.len() {
            return false;
        }
        let mut target_plan = source_plan;
        for (planned, replacement) in target_plan.iter_mut().zip(operands) {
            planned.expression = *replacement;
        }
        self.entries.insert(target, target_plan).is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspend_rewrites_preserve_the_exact_plan_and_abi_boundary() {
        let mut plans = DefaultCallOperands::default();
        plans.record(
            7,
            vec![
                DefaultCallOperand::supplied(10),
                DefaultCallOperand::synthesized(11),
                DefaultCallOperand::synthesized_abi(12),
                DefaultCallOperand::synthesized_abi(13),
            ],
        );

        assert_eq!(plans.insert_continuation(7, 99), Ok(Some(2)));
        assert!(plans.matching(7, &[10, 11, 99, 12, 13]).is_some());

        assert!(plans.replace_operands(7, &[20, 21, 29, 22, 23]));
        assert!(plans.matching(7, &[20, 21, 29, 22, 23]).is_some());

        assert!(plans.clone_call(7, 8, &[30, 31, 39, 32, 33]));
        assert!(plans.matching(8, &[30, 31, 39, 32, 33]).is_some());
        assert!(!plans.replace_operands(8, &[30]));
    }
}
