//! Coroutine suspension points common IR retains: the non-call ones kotlinc expands itself, and
//! how a value-class result crosses one.

use super::{ExprId, IrExpr, IrFile, Ty};
use crate::types::TypeName;

/// Representation selected for a value-class result crossing a coroutine suspension boundary.
/// A carrier that cannot preserve the value class's null semantics in an erased `Object` is wrapped;
/// a directly representable carrier crosses unchanged. This is produced by a target value-class pass
/// and consumed by its coroutine pass, after common lowering has finished.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrValueClassSuspendResult {
    Boxed { classifier: TypeName, carrier: Ty },
    Carrier { classifier: TypeName, carrier: Ty },
}

impl IrValueClassSuspendResult {
    /// Type physically present in the continuation's erased result slot before the already-lowered
    /// call-site representation wrapper consumes it.
    pub fn boundary_ty(self) -> Ty {
        match self {
            Self::Boxed { classifier, .. } => Ty::obj_name(classifier),
            Self::Carrier { carrier, .. } => carrier,
        }
    }
}

/// One non-call coroutine suspension point retained through common IR: a
/// `suspendCoroutineUninterceptedOrReturn` block, invoked with the current continuation directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrIntrinsicSuspensionPoint {
    pub result: Ty,
}

impl IrFile {
    /// Build the checked invocation that enters a `suspendCoroutineUninterceptedOrReturn` block.
    /// The same source operation is used by FIR lowering and selected KLIB inline templates.
    pub(crate) fn invoke_with_current_continuation(&mut self, block: ExprId) -> Option<ExprId> {
        let IrExpr::Lambda {
            impl_fn,
            captures,
            arity: 1,
            inline_body: Some(_),
            ..
        } = self.expr(block)
        else {
            return None;
        };
        let implementation = self.functions.get(*impl_fn as usize)?;
        if implementation.params.len() != captures.len() + 1 {
            return None;
        }
        let continuation_ty = implementation.params[captures.len()];
        let body_result = implementation.ret;
        let continuation = self.add_expr(IrExpr::CurrentContinuation);
        self.set_logical_type(continuation, continuation_ty);
        Some(self.add_expr(IrExpr::InvokeFunction {
            func: block,
            args: vec![continuation],
            params: vec![continuation_ty],
            ret: body_result,
        }))
    }

    /// Record the selected logical result of the atomic continuation-block invocation.
    pub(crate) fn mark_unintercepted_suspension(&mut self, invocation: ExprId, result: Ty) {
        self.intrinsic_suspension_points
            .insert(invocation, IrIntrinsicSuspensionPoint { result });
    }

    /// Whether `e` is a `suspendCoroutineUninterceptedOrReturn` block, which reads the caller's
    /// own continuation.
    pub fn is_unintercepted_suspension(&self, e: ExprId) -> bool {
        self.intrinsic_suspension_points.contains_key(&e)
    }
}
