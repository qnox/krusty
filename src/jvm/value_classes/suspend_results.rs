//! How a suspend function's value-class result crosses the erased `Continuation` boundary.
//!
//! kotlinc returns a non-null value-class result unboxed when its carrier is a reference, a
//! nullable reference included, and no declaration it overrides returns another classifier. A
//! scalar carrier, and a nullable value class whose ordinary erasure is the box, cross boxed. A
//! call receives what its callee returns, so a call to a callee that returns a type parameter
//! receives the box and unboxes it, whatever the call's own result type.

use super::{erase, is_ref, nullable_is_boxed, Under};
use crate::fir::ResolvedFunctionOverrideTarget;
use crate::ir::{IrExpr, IrFile, IrTypeOp, IrValueClassSuspendResult};
use crate::types::{Ty, TypeName};
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
    // A suspend lambda's `invoke` erases its result to `Object`, so its implementation returns the
    // value class boxed, as every lambda does, unless its SAM method declares that very value class.
    let lambdas = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
            _ => None,
        })
        .filter(|&function| !super::sam_declares_vc_return(ir, declared_results, function, under))
        .collect::<HashSet<_>>();
    // This includes nullable value classes: `X<String>?` can use `String` itself as the nullable
    // carrier, whereas `X<Int>?` must remain the boxed `X` because an `int` cannot represent null.
    for &function in suspend_functions {
        if let Some(realization) = declared_results.get(function as usize).and_then(|result| {
            suspend_result_representation(result, under, forced.contains(&function))
        }) {
            let realization = match lambdas.contains(&function) {
                true => boxed(realization),
                false => realization,
            };
            ir.value_class_suspend_returns.insert(function, realization);
        }
    }
    // A call to a dependency receives the representation its applied result selects: the box when
    // the callee overrides a declaration returning another classifier, as a module override does.
    let external = ir.suspend_calls.iter().filter_map(|(&call, result)| {
        let overrides_another = result.non_null().obj_internal().is_some_and(|classifier| {
            ir.suspend_call_overridden_results
                .get(&call)
                .is_some_and(|overridden| {
                    overridden
                        .iter()
                        .any(|&overridden| declares_another_classifier(overridden, classifier))
                })
        });
        suspend_result_representation(result, under, overrides_another)
            .map(|realization| (call, realization))
    });
    // A callee that declares a type parameter, or another classifier, as its result completes with
    // the box on either path, in this module or a dependency; the call's checked coercion names the
    // value class it unboxes to.
    let generic = ir.exprs.iter().filter_map(|expression| {
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg: call,
            type_operand,
        } = *expression
        else {
            return None;
        };
        let declared = match crate::jvm::suspend::suspend_call_fid(ir, call, suspend_functions) {
            Some(callee) => declared_results.get(callee as usize)?,
            None if ir.suspend_calls.contains_key(&call) => ir.call_declared_ret.get(&call)?,
            None => return None,
        };
        if suspend_result_representation(declared, under, false).is_some() {
            return None;
        }
        suspend_result_representation(&type_operand, under, false)
            .map(|result| (call, boxed(result)))
    });
    let calls = external.chain(generic).collect::<Vec<_>>();
    ir.value_class_suspend_calls.extend(calls);
    keep_handed_over_boxes(ir);
    forced
}

/// A call that hands over its value class's box already is the value its checked coercion to that
/// class names. Keep the box, as a call to a same-module callee returning the box does, so each
/// consumer takes the carrier at its own boundary and a reference consumer, `Any` included, keeps
/// the box it was handed.
fn keep_handed_over_boxes(ir: &mut IrFile) {
    let coercions = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(id, expression)| match *expression {
            IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: call,
                type_operand,
            } if !type_operand.is_nullable()
                && matches!(
                    ir.value_class_suspend_calls.get(&call),
                    Some(IrValueClassSuspendResult::Boxed { classifier, .. })
                        if type_operand.obj_internal() == Some(*classifier)
                ) =>
            {
                Some((id, call))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    for (coercion, call) in coercions {
        ir.exprs[coercion] = IrExpr::Block {
            stmts: Vec::new(),
            value: Some(call),
        };
    }
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
/// overridden declaration, in this module or a dependency, returns another classifier, a type
/// parameter included. Each bridge and override edge carries the exact selected target; emitted
/// names are never lookup input.
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
    let overridden = ir.function_overrides.values().flatten().filter_map(|edge| {
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
        declares_another_classifier(edge.declared_result, classifier).then_some(function)
    });
    bridged.chain(overridden).collect()
}

/// Whether an overridden declaration's result names another classifier than `classifier`, a type
/// parameter included: a type parameter has no classifier of its own, whatever its bound.
fn declares_another_classifier(overridden: Ty, classifier: TypeName) -> bool {
    let overridden = match overridden.non_null() {
        Ty::TyParam(..) => None,
        result => result.obj_internal(),
    };
    overridden != Some(classifier)
}

/// Select the physical result carried through a suspend function's erased `Object` boundary.
///
/// The declared type remains the semantic identity used for overloads and metadata. This target pass
/// records only how that already-selected value crosses CPS. A scalar carrier requires a box. A
/// reference carrier, a null-capable one included, crosses directly unless an exact override edge
/// requires the concrete value-class identity at the supertype boundary: `X(null)` is that
/// carrier's null, and a non-null result is not also a missing value. A nullable value class keeps
/// its ordinary erasure, so `X?` crosses as the box exactly when `X?` is boxed.
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
    let crosses_as_box = if declared.is_nullable() {
        nullable_is_boxed(classifier, under)
    } else {
        force_boxed || !underlying_is_reference(classifier, under)
    };
    if crosses_as_box {
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

/// Whether `classifier`'s underlying erases to a JVM reference. A nullable reference counts: its
/// null is a legal carrier of the non-null value class (`X(null)`).
fn underlying_is_reference(classifier: TypeName, under: &Under) -> bool {
    under
        .get(&classifier)
        .is_some_and(|underlying| is_ref(&erase(underlying, under)))
}
