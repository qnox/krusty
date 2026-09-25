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

/// A suspension point of a function whose machine kotlinc's transformer builds from its bytecode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TransformedSuspension {
    /// The call expression. Its continuation operand is an `IrExpr::CurrentContinuation`, the
    /// function's `$completion` parameter; the transformer replaces it with the machine's own.
    pub call: ExprId,
    /// The callee's declared result, which the call's erased `Object` result is coerced to.
    pub result: crate::types::Ty,
}

/// A function whose machine kotlinc's coroutine transformer builds from its bytecode.
#[derive(Clone, Debug)]
pub(crate) struct TransformedMachine {
    /// The internal name of its continuation class.
    pub continuation_class: String,
    /// Its suspension points, in encounter order.
    pub suspensions: Vec<TransformedSuspension>,
}

/// The suspend functions whose machine is built during emission, with their suspensions: those
/// the emit-time machine builds around a spliced inline body, and those kotlinc's transformer
/// builds when the class is written.
#[derive(Default)]
pub(crate) struct EmitTimeMachines {
    functions: HashMap<u32, Vec<SplicedSuspension>>,
    transformed: HashMap<u32, TransformedMachine>,
}

impl EmitTimeMachines {
    pub(crate) fn record(&mut self, function: u32, suspensions: Vec<SplicedSuspension>) {
        self.functions.insert(function, suspensions);
    }

    pub(crate) fn suspensions(&self, function: u32) -> Option<&[SplicedSuspension]> {
        self.functions.get(&function).map(Vec::as_slice)
    }

    pub(crate) fn record_transformed(&mut self, function: u32, machine: TransformedMachine) {
        self.transformed.insert(function, machine);
    }

    pub(crate) fn transformed(&self, function: u32) -> Option<&TransformedMachine> {
        self.transformed.get(&function)
    }
}
