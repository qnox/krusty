//! Which same-module inline calls a backend inlines from the callee's compiled body.
//!
//! Common lowering expands a call to a same-module inline function by cloning the callee's checked
//! IR template at the call. A backend that compiles each inline function to a method body first
//! (the JVM, as kotlinc's `IrSourceCompilerForInline` does) can instead inline that body, so the
//! inlined code keeps the callee's erased types. The backend states which realization it wants
//! before any body is lowered ([`SameModuleInlining`]); common lowering then decides each call's
//! route up front and records the calls it keeps as calls for the backend to inline.

use crate::fx_hash::HashMap;

use super::{ExprId, FunId, IrExpr, IrFile};

/// How common lowering realizes the calls of a file to same-module inline functions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SameModuleInlining {
    /// Every call is expanded in IR from the callee's template.
    #[default]
    ExpandInIr,
    /// A call whose shape the backend inlines from the callee's compiled body stays a call, recorded
    /// in [`IrFile::compiled_inline_call`]; every other call is expanded in IR.
    CompiledBodies,
}

#[derive(Default)]
pub(super) struct CompiledInlineCalls {
    lowering: SameModuleInlining,
    /// Each call kept for the backend → the function whose compiled body it inlines.
    calls: HashMap<ExprId, FunId>,
}

impl IrFile {
    /// How this file's calls to same-module inline functions are lowered.
    pub fn same_module_inlining(&self) -> SameModuleInlining {
        self.compiled_inline_calls.lowering
    }

    /// Select how this file's calls to same-module inline functions are lowered. Set once, before
    /// any body is lowered.
    pub fn set_same_module_inlining(&mut self, lowering: SameModuleInlining) {
        self.compiled_inline_calls.lowering = lowering;
    }

    /// Record that `call` is inlined from the compiled body of `function`.
    pub(crate) fn record_compiled_inline_call(&mut self, call: ExprId, function: FunId) {
        debug_assert_eq!(
            self.compiled_inline_calls.lowering,
            SameModuleInlining::CompiledBodies
        );
        self.compiled_inline_calls.calls.insert(call, function);
    }

    /// The function whose compiled body `call` inlines, when lowering kept it as a call for that.
    pub fn compiled_inline_call(&self, call: ExprId) -> Option<FunId> {
        self.compiled_inline_calls.calls.get(&call).copied()
    }

    /// Every call of this file inlined from a compiled body, with the function it inlines.
    pub fn compiled_inline_calls(&self) -> impl Iterator<Item = (ExprId, FunId)> + '_ {
        self.compiled_inline_calls
            .calls
            .iter()
            .map(|(&call, &function)| (call, function))
    }

    /// A recorded call moved to `target` (below a representation wrapper left at `source`) is
    /// still the call inlined from that body; the wrapper is not.
    pub(crate) fn move_compiled_inline_call(&mut self, source: ExprId, target: ExprId) {
        if let Some(function) = self.compiled_inline_calls.calls.remove(&source) {
            self.compiled_inline_calls.calls.insert(target, function);
        }
    }

    /// The call `expression` evaluates (below any type operation) when one of its operands is a
    /// call inlined from a compiled body. kotlinc forgets the line in effect after an inlined body,
    /// so that call marks its own line again at its invoke.
    pub(crate) fn call_after_compiled_inline_operand(&self, expression: ExprId) -> Option<ExprId> {
        let call = self.below_type_operations(expression);
        let IrExpr::Call {
            dispatch_receiver,
            args,
            ..
        } = self.expr(call)
        else {
            return None;
        };
        dispatch_receiver
            .iter()
            .chain(args)
            .any(|&operand| {
                self.compiled_inline_call(self.below_type_operations(operand))
                    .is_some()
            })
            .then_some(call)
    }

    fn below_type_operations(&self, mut expression: ExprId) -> ExprId {
        while let IrExpr::TypeOp { arg, .. } = self.expr(expression) {
            expression = *arg;
        }
        expression
    }

    /// A copy of a recorded call is inlined from the same body.
    pub(super) fn copy_compiled_inline_call(&mut self, source: ExprId, target: ExprId) {
        if let Some(function) = self.compiled_inline_call(source) {
            self.compiled_inline_calls.calls.insert(target, function);
        }
    }
}
