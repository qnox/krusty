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
}

impl DefaultCallOperand {
    pub(super) fn supplied(expression: ExprId) -> Self {
        Self {
            expression,
            origin: DefaultOperandOrigin::Supplied,
        }
    }

    pub(super) fn synthesized(expression: ExprId) -> Self {
        Self {
            expression,
            origin: DefaultOperandOrigin::Synthesized,
        }
    }
}

#[derive(Default)]
pub(crate) struct DefaultCallOperands {
    entries: HashMap<ExprId, Box<[DefaultCallOperand]>>,
}

impl DefaultCallOperands {
    pub(super) fn record(&mut self, call: ExprId, operands: Vec<DefaultCallOperand>) {
        let previous = self.entries.insert(call, operands.into_boxed_slice());
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
        self.entries.get(&call).map(Box::as_ref).filter(|plan| {
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
}
