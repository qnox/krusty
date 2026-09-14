//! Checked bottom-value completion at JVM suspension points.

use super::is_suspension_point;
use crate::ir::{ExprId, IrBottomValueCompletion, IrExpr, IrFile, IrTypeOp};
use std::collections::HashSet;

#[derive(Clone, Copy)]
pub(super) struct UnwrappedSuspension {
    pub(super) point: ExprId,
    pub(super) completion: Option<IrBottomValueCompletion>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SuspensionCompletion {
    /// The raw call's checked result is semantic bottom, so its pre-CPS logical marker must not be
    /// interpreted as the physical `Object` result of the rewritten call.
    pub(super) semantic_bottom: bool,
    /// The resumed path terminates in this exact use context. A substituted generic bottom value
    /// used as a statement is semantic bottom but deliberately falls through after being discarded.
    pub(super) resume_diverges: bool,
}

/// Peel checked result wrappers off `e` when their producer is a suspend call, returning the
/// underlying call and any bottom completion; otherwise return `e` unchanged. A generic suspend
/// member call (`suspend fun findAll(): List<T>`) has its erased `Object` result cast to the declared
/// type at the call site. The coroutine flattener binds the raw call and re-applies the cast via
/// `bind_from_r`, so the wrapper must be seen through to recognize the suspension.
///
/// `ref_only` restricts the peel to a reference `Cast` (a redundant checkcast on the erased `Object`):
/// a tail-forward returns the callee's `Object` result verbatim with no re-coercion, so an
/// `ImplicitCoercion` that boxes a primitive result must be kept. Dropping it would `areturn` an
/// unboxed value where a reference is required. The flattener path re-applies the coercion via
/// `bind_from_r`, so it peels both.
pub(super) fn unwrap_suspend_cast(
    ir: &IrFile,
    e: ExprId,
    suspend_set: &HashSet<u32>,
    ref_only: bool,
) -> UnwrappedSuspension {
    let ops: &[IrTypeOp] = if ref_only {
        &[IrTypeOp::Cast]
    } else {
        &[IrTypeOp::Cast, IrTypeOp::ImplicitCoercion]
    };
    let original = e;
    let mut peeled = e;
    let mut completion = None;
    loop {
        match ir.exprs[peeled as usize] {
            IrExpr::TypeOp { op, arg, .. } if ops.contains(&op) => peeled = arg,
            IrExpr::BottomValue {
                producer,
                completion: selected,
            } => {
                // A tail-forward must expose the physical suspend call so the caller's
                // continuation owns completion. It deliberately returns the callee's Object
                // result verbatim, so the checked bottom completion does not run in this frame.
                // The state-machine path keeps the exact completion for its resume state.
                if !ref_only && completion.is_none() {
                    completion = Some(selected);
                }
                peeled = producer;
            }
            _ => break,
        }
    }
    if is_suspension_point(ir, peeled, suspend_set) {
        UnwrappedSuspension {
            point: peeled,
            completion,
        }
    } else {
        UnwrappedSuspension {
            point: original,
            completion: None,
        }
    }
}

pub(super) fn suspension_completion(
    suspension: UnwrappedSuspension,
    discarded: bool,
) -> SuspensionCompletion {
    let Some(completion) = suspension.completion else {
        return SuspensionCompletion::default();
    };
    SuspensionCompletion {
        semantic_bottom: true,
        resume_diverges: !discarded || completion.diverges_when_discarded(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    #[test]
    fn suspension_unwrap_preserves_the_exact_resume_completion() {
        let mut ir = IrFile::with_package(None);
        let point = ir.add_expr(IrExpr::UnitInstance);
        ir.suspend_calls.insert(point, Ty::Nothing);
        let fallthrough = ir.add_expr(IrExpr::BottomValue {
            producer: point,
            completion: IrBottomValueCompletion::FallThroughWhenDiscarded,
        });
        let divergent = ir.add_expr(IrExpr::BottomValue {
            producer: point,
            completion: IrBottomValueCompletion::Diverge,
        });
        let suspend_set = HashSet::new();

        let discarded = unwrap_suspend_cast(&ir, fallthrough, &suspend_set, false);
        assert_eq!(discarded.point, point);
        assert_eq!(
            discarded.completion,
            Some(IrBottomValueCompletion::FallThroughWhenDiscarded)
        );
        assert_eq!(
            suspension_completion(discarded, true),
            SuspensionCompletion {
                semantic_bottom: true,
                resume_diverges: false,
            }
        );
        assert_eq!(
            suspension_completion(discarded, false),
            SuspensionCompletion {
                semantic_bottom: true,
                resume_diverges: true,
            }
        );

        let discarded = unwrap_suspend_cast(&ir, divergent, &suspend_set, false);
        assert_eq!(
            suspension_completion(discarded, true),
            SuspensionCompletion {
                semantic_bottom: true,
                resume_diverges: true,
            }
        );

        let plain = unwrap_suspend_cast(&ir, point, &suspend_set, false);
        assert_eq!(
            suspension_completion(plain, false),
            SuspensionCompletion::default()
        );
    }

    #[test]
    fn tail_forward_exposes_the_call_without_claiming_its_bottom_completion() {
        let mut ir = IrFile::with_package(None);
        let point = ir.add_expr(IrExpr::UnitInstance);
        ir.suspend_calls.insert(point, Ty::Nothing);
        let wrapped = ir.add_expr(IrExpr::BottomValue {
            producer: point,
            completion: IrBottomValueCompletion::Diverge,
        });

        let tail = unwrap_suspend_cast(&ir, wrapped, &HashSet::new(), true);
        assert_eq!(tail.point, point);
        assert_eq!(tail.completion, None);
    }
}
