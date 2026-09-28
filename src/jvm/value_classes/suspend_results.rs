//! How a suspend function's value-class result crosses the erased `Continuation` boundary.
//!
//! kotlinc returns a value-class result unboxed only when the function's own declared result is
//! that value class and no declaration it overrides returns another classifier; otherwise the box
//! crosses. A call receives what its callee returns, so a call to a callee that returns a type
//! parameter receives the box and unboxes it, whatever the call's own result type.

use super::{erase, nullable_is_boxed, Under};
use crate::fir::ResolvedFunctionOverrideTarget;
use crate::ir::{IrExpr, IrFile, IrTypeOp, IrValueClassSuspendResult};
use crate::types::Ty;
use std::collections::HashSet;

/// Record the representation of every suspend function's result and of every suspend call's
/// result. Returns the suspend functions whose value-class result an override forces into its box.
pub(super) fn record_suspend_results(
    ir: &mut IrFile,
    under: &Under,
    declared_results: &[Ty],
    suspend_functions: &HashSet<u32>,
) -> HashSet<u32> {
    let forced = force_boxed_results(ir, under, declared_results, suspend_functions);
    // This includes nullable value classes: `X<String>?` can use `String` itself as the nullable
    // carrier, whereas `X<Int>?` must remain the boxed `X` because an `int` cannot represent null.
    for &function in suspend_functions {
        if let Some(realization) = declared_results.get(function as usize).and_then(|result| {
            suspend_result_representation(result, under, forced.contains(&function))
        }) {
            ir.value_class_suspend_returns.insert(function, realization);
        }
    }
    // A call to a dependency receives the representation its applied result selects.
    let external = ir.suspend_calls.iter().filter_map(|(&call, result)| {
        suspend_result_representation(result, under, false).map(|realization| (call, realization))
    });
    // A same-module callee that returns a type parameter completes with the box on either path; the call's checked coercion names the value class it unboxes to.
    let generic = ir.exprs.iter().filter_map(|expression| {
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand,
        } = *expression
        else {
            return None;
        };
        let callee = crate::jvm::suspend::suspend_call_fid(ir, call, suspend_functions)?;
        let declared = declared_results.get(callee as usize)?;
        if suspend_result_representation(declared, under, false).is_some() {
            return None;
        }
        suspend_result_representation(&type_operand, under, false)
            .map(|result| (call, boxed(result)))
    });
    let calls = external.chain(generic).collect::<Vec<_>>();
    ir.value_class_suspend_calls.extend(calls);
    forced
}

/// The box of a value class a callee hands over as its type parameter's (or supertype's) value.
fn boxed(result: IrValueClassSuspendResult) -> IrValueClassSuspendResult {
    match result {
        IrValueClassSuspendResult::Boxed { .. } => result,
        IrValueClassSuspendResult::Carrier {
            classifier,
            carrier,
        } => IrValueClassSuspendResult::Boxed {
            classifier,
            carrier,
        },
    }
}

/// Suspend overrides whose value-class result crosses as its box. A supertype that observes a
/// non-value-class result through a bridge needs the concrete value class's box across `Object`,
/// even when its carrier happens to have the same JVM descriptor; and kotlinc boxes whenever an
/// overridden module declaration returns another classifier, a type parameter included. Each
/// bridge and override edge carries the exact selected target; emitted names are never lookup input.
fn force_boxed_results(
    ir: &IrFile,
    under: &Under,
    declared_results: &[Ty],
    suspend_functions: &HashSet<u32>,
) -> HashSet<u32> {
    let bridged = ir
        .classes
        .iter()
        .flat_map(|class| class.bridges.iter())
        .filter_map(|bridge| {
            let target = bridge
                .target_function
                .filter(|target| suspend_functions.contains(target))?;
            let classifier = bridge
                .concrete_ret
                .non_null()
                .obj_internal()
                .filter(|classifier| under.contains_key(classifier))?;
            let supertype_uses_same_value_class_carrier = bridge
                .erased_ret
                .non_null()
                .obj_internal()
                .is_some_and(|result| result == classifier)
                && (!bridge.erased_ret.is_nullable() || !nullable_is_boxed(classifier, under));
            (!supertype_uses_same_value_class_carrier).then_some(target)
        });
    let overridden = ir
        .function_overrides
        .values()
        .flatten()
        .filter(|edge| matches!(edge.overridden, ResolvedFunctionOverrideTarget::Module(_)))
        .filter_map(|edge| {
            let function = edge
                .implementation_function
                .or_else(|| match edge.implementation {
                    ResolvedFunctionOverrideTarget::Module(declaration) => {
                        ir.checked_callable_functions.get(&declaration).copied()
                    }
                    ResolvedFunctionOverrideTarget::External(_) => None,
                })?;
            if !suspend_functions.contains(&function) {
                return None;
            }
            let classifier = declared_results
                .get(function as usize)?
                .non_null()
                .obj_internal()
                .filter(|classifier| under.contains_key(classifier))?;
            // A type parameter has no classifier of its own, whatever its bound.
            let overridden_classifier = match edge.declared_result.non_null() {
                Ty::TyParam(..) => None,
                result => result.obj_internal(),
            };
            (overridden_classifier != Some(classifier)).then_some(function)
        });
    bridged.chain(overridden).collect()
}

/// Select the physical result carried through a suspend function's erased `Object` boundary.
///
/// The declared type remains the semantic identity used for overloads and metadata. This target pass
/// records only how that already-selected value crosses CPS. Scalar and null-capable carriers require a
/// box; a non-null reference carrier crosses directly unless an exact override edge requires the concrete
/// value-class identity at the supertype boundary. Nullable value classes keep their ordinary erasure.
pub(super) fn suspend_result_representation(
    declared: &Ty,
    under: &Under,
    force_boxed: bool,
) -> Option<IrValueClassSuspendResult> {
    let classifier = declared
        .non_null()
        .obj_internal()
        .filter(|classifier| under.contains_key(classifier))?;
    let carrier = erase(declared, under);
    if !declared.is_nullable() && (force_boxed || nullable_is_boxed(classifier, under)) {
        Some(IrValueClassSuspendResult::Boxed {
            classifier,
            carrier,
        })
    } else {
        Some(IrValueClassSuspendResult::Carrier {
            classifier,
            carrier,
        })
    }
}
