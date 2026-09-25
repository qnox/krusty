//! JVM-owned provenance for the physical operands of checked calls.
//!
//! Common IR retains supplied arguments and omitted semantic parameter ordinals. JVM realization
//! materializes placeholders, mask words, and the marker. This side table keeps that physical plan
//! beside the backend, keyed by the stable call expression. Suspend lowering also records the exact
//! operand position into which it inserts a CPS continuation. Emission therefore never guesses
//! provenance from a zero/null constant, a type or name, and common IR never carries JVM ABI facts.

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
    /// Exact physical argument occupied by the CPS continuation after suspend lowering. This is a
    /// backend representation fact: emission must not rediscover it from the argument's type,
    /// spelling, or position relative to a descriptor suffix.
    continuation_positions: HashMap<ExprId, usize>,
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

    /// The recorded boundary before the JVM-only mask/marker suffix. A suspend continuation is
    /// inserted at this exact position; consumers must not recover it from descriptor spelling.
    pub(super) fn abi_suffix_position(&self, call: ExprId) -> Option<usize> {
        self.entries
            .get(&call)?
            .iter()
            .position(|operand| operand.abi_suffix)
    }

    pub(super) fn record_continuation(&mut self, call: ExprId, position: usize) -> bool {
        match self.continuation_positions.entry(call) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(position);
                true
            }
            std::collections::hash_map::Entry::Occupied(_) => false,
        }
    }

    pub(super) fn is_continuation(&self, call: ExprId, position: usize) -> bool {
        self.continuation_positions.get(&call) == Some(&position)
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
        if self
            .continuation_positions
            .get(&call)
            .is_some_and(|position| *position >= operands.len())
        {
            return false;
        }
        if let Some(plan) = self.entries.get_mut(&call) {
            if plan.len() != operands.len() {
                return false;
            }
            for (planned, replacement) in plan.iter_mut().zip(operands) {
                planned.expression = *replacement;
            }
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
        for (&call, &position) in &self.continuation_positions {
            let present = match ir.exprs.get(call as usize) {
                Some(crate::ir::IrExpr::Call { args, .. })
                | Some(crate::ir::IrExpr::InvokeFunction { args, .. }) => position < args.len(),
                Some(crate::ir::IrExpr::MethodCall { args, .. }) => {
                    args.get(position).is_some_and(Option::is_some)
                }
                _ => return false,
            };
            if !present {
                return false;
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
        let source_plan = self.entries.get(&source).cloned();
        let continuation_position = self.continuation_positions.get(&source).copied();
        if source_plan.is_none() && continuation_position.is_none() {
            return true;
        }
        if source_plan
            .as_ref()
            .is_some_and(|plan| plan.len() != operands.len())
            || continuation_position.is_some_and(|position| position >= operands.len())
            || self.entries.contains_key(&target)
            || self.continuation_positions.contains_key(&target)
        {
            return false;
        }
        if let Some(mut target_plan) = source_plan {
            for (planned, replacement) in target_plan.iter_mut().zip(operands) {
                planned.expression = *replacement;
            }
            self.entries.insert(target, target_plan);
        }
        if let Some(position) = continuation_position {
            self.continuation_positions.insert(target, position);
        }
        true
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

        assert_eq!(plans.abi_suffix_position(7), Some(2));
        assert_eq!(plans.insert_continuation(7, 99), Ok(Some(2)));
        assert!(plans.record_continuation(7, 2));
        assert!(plans.matching(7, &[10, 11, 99, 12, 13]).is_some());
        assert!(plans.is_continuation(7, 2));

        assert!(plans.replace_operands(7, &[20, 21, 29, 22, 23]));
        assert!(plans.matching(7, &[20, 21, 29, 22, 23]).is_some());

        assert!(plans.clone_call(7, 8, &[30, 31, 39, 32, 33]));
        assert!(plans.matching(8, &[30, 31, 39, 32, 33]).is_some());
        assert!(plans.is_continuation(8, 2));
        assert!(!plans.replace_operands(8, &[30]));
    }

    #[test]
    fn an_ordinary_suspend_call_keeps_its_exact_continuation_position() {
        let mut plans = DefaultCallOperands::default();
        assert!(plans.record_continuation(3, 1));
        assert!(plans.is_continuation(3, 1));
        assert!(!plans.is_continuation(3, 0));
        assert!(plans.replace_operands(3, &[20, 21]));
        assert!(plans.clone_call(3, 4, &[30, 31]));
        assert!(plans.is_continuation(4, 1));
    }
}
