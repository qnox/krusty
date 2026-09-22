//! Which suspend functions get their state machine at emission instead of in IR.
//!
//! The routing gate is deliberately a conjunction (see `inline_suspension`): a function the IR
//! machine found no suspension point in, which nevertheless has one inside a spliced inline body.
//! That is exactly the function that would otherwise be emitted with no continuation to pass, so
//! claiming it can only replace a `call arity mismatch` — never take over a function the IR machine
//! already compiles.

use crate::ir::ExprId;
use std::collections::HashMap;

/// One suspension whose machine is built during emission, in the enclosing body's encounter order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SplicedSuspension {
    /// The call expression. Its continuation operand is an `IrExpr::CurrentContinuation`, which
    /// resolves to the `$completion` parameter while discovering and to the machine's own
    /// continuation local while emitting.
    pub call: ExprId,
}

/// The suspend functions whose machine is built during emission, with their suspensions.
#[derive(Default)]
pub(crate) struct EmitTimeMachines {
    functions: HashMap<u32, Vec<SplicedSuspension>>,
}

impl EmitTimeMachines {
    pub(crate) fn record(&mut self, function: u32, suspensions: Vec<SplicedSuspension>) {
        self.functions.insert(function, suspensions);
    }

    pub(crate) fn suspensions(&self, function: u32) -> Option<&[SplicedSuspension]> {
        self.functions.get(&function).map(Vec::as_slice)
    }
}
