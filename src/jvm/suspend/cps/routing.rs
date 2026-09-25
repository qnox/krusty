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
    /// The internal name of its continuation class: for a suspend lambda, the lambda's own class.
    pub continuation_class: String,
    /// Its suspension points, in encounter order.
    pub suspensions: Vec<TransformedSuspension>,
    /// Set for a suspend lambda's `invokeSuspend`, which the transformer takes in its lambda mode.
    pub lambda: Option<SuspendLambdaMachine>,
}

/// What the transformer's lambda mode needs from a suspend lambda's `invokeSuspend` beyond its
/// suspension points.
#[derive(Clone, Debug)]
pub(crate) struct SuspendLambdaMachine {
    /// The declarations that read the lambda's parameters back from their fields at the top of the
    /// body. codegen marks each with `mark(10)`, after which the coroutine starts.
    pub parameter_reads: Vec<ExprId>,
    /// The spill fields the class declares for those parameters, by normalized descriptor, with the
    /// highest index of each (kotlinc's `continuationClassVarsCountByType`), in declaration order.
    pub declared_spill_fields: Vec<(String, usize)>,
}

/// A value a suspend lambda's class captures.
#[derive(Clone, Debug)]
pub(crate) struct SuspendLambdaCapture {
    /// Its field, by index into the class's `IrClass::fields`.
    pub field: u32,
    /// The captured value's type.
    pub ty: crate::types::Ty,
    /// Whether it is the enclosing class's `this`, which the constructor names `$receiver`.
    pub receiver: bool,
}

/// A suspend lambda realized as a class of its own (kotlinc's `SuspendLambdaLowering`), which the
/// emitter writes with the members kotlinc declares for it.
#[derive(Clone, Debug)]
pub(crate) struct SuspendLambdaClass {
    /// The class's `invokeSuspend`, the lambda's body.
    pub invoke_suspend: u32,
    /// The lambda's Kotlin function type.
    pub function_type: crate::types::Ty,
    /// The captured values, in constructor order.
    pub captures: Vec<SuspendLambdaCapture>,
    /// The lambda's own parameters (its receiver first), in order: each one's type, and the field
    /// it is kept in when the body reads it.
    pub parameters: Vec<(crate::types::Ty, Option<u32>)>,
}

/// The suspend functions whose machine is built during emission, with their suspensions: those
/// the emit-time machine builds around a spliced inline body, and those kotlinc's transformer
/// builds when the class is written.
#[derive(Default)]
pub(crate) struct EmitTimeMachines {
    functions: HashMap<u32, Vec<SplicedSuspension>>,
    transformed: HashMap<u32, TransformedMachine>,
    suspend_lambdas: HashMap<crate::types::TypeName, SuspendLambdaClass>,
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

    pub(crate) fn record_suspend_lambda(
        &mut self,
        class: crate::types::TypeName,
        lambda: SuspendLambdaClass,
    ) {
        self.suspend_lambdas.insert(class, lambda);
    }

    /// The suspend lambda `class` realizes, when it is one.
    pub(crate) fn suspend_lambda(
        &self,
        class: crate::types::TypeName,
    ) -> Option<&SuspendLambdaClass> {
        self.suspend_lambdas.get(&class)
    }
}
