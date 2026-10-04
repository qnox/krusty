//! Coroutine suspension points common IR retains: the non-call ones kotlinc expands itself, and
//! how a value-class result crosses one.

use super::{ExprId, IrFile, Ty};
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

/// Semantic behavior of one non-call coroutine suspension point retained through common IR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrIntrinsicSuspensionKind {
    /// Invoke the block with the current continuation directly.
    Unintercepted,
    /// Invoke the block with Kotlin's one-shot safe, intercepted continuation.
    Safe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrIntrinsicSuspensionPoint {
    pub result: Ty,
    pub kind: IrIntrinsicSuspensionKind,
}

impl IrFile {
    /// Whether `e` is a `suspendCoroutineUninterceptedOrReturn` block, which reads the caller's
    /// own continuation.
    pub fn is_unintercepted_suspension(&self, e: ExprId) -> bool {
        self.intrinsic_suspension_points
            .get(&e)
            .is_some_and(|point| point.kind == IrIntrinsicSuspensionKind::Unintercepted)
    }
}
