//! JVM CPS operand realization for calls selected before suspend lowering.
//!
//! This boundary owns the physical continuation slot and publishes its exact position to emission.
//! Neither the emitter nor later transforms recover that role from a type, name, or operand shape.

use super::{continuation_ty, object_ty};
use crate::ir::{Callee, ExprId, IrExpr, IrFile};
use crate::types::Ty;

/// The CPS form of a logical method descriptor: append the trailing `Continuation` parameter and
/// erase the return to `Object`. A cross-unit suspend callee is resolved by its logical signature,
/// but the emitted invocation must name its physical CPS descriptor.
fn cps_descriptor(logical: &str) -> String {
    let close = logical
        .rfind(')')
        .unwrap_or(logical.len().saturating_sub(1));
    format!(
        "{}Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
        &logical[..close]
    )
}

/// Locate the continuation slot in a suspend `$default` descriptor. Its ABI suffix is
/// `Continuation, int mask..., Object marker`; scanning the typed signature keeps both continuation
/// insertion and operand spilling independent of source arity and of the number of mask words.
pub(super) fn default_suspend_continuation_index(params: &[Ty]) -> Option<usize> {
    let mut index = params.len().checked_sub(2)?;
    let mut masks = 0;
    while params.get(index).copied() == Some(Ty::Int) {
        masks += 1;
        index = index.checked_sub(1)?;
    }
    (masks > 0
        && params
            .get(index)
            .and_then(|ty| ty.obj_internal())
            .is_some_and(|name| name.matches("kotlin/coroutines/Continuation")))
    .then_some(index)
}

/// Append the CPS continuation expected by the already-selected callable and record the physical
/// operand role. A non-call intrinsic owns its continuation internally and remains unchanged.
pub(super) fn append_continuation(
    ir: &mut IrFile,
    call: ExprId,
    continuation: ExprId,
    operand_provenance: &mut crate::jvm::default_call_operands::DefaultCallOperands,
) -> bool {
    crate::trace_compiler!(
        "suspend",
        "append continuation call={call} continuation={continuation} node={:?}",
        ir.exprs.get(call as usize),
    );
    let planned_index = match operand_provenance.insert_continuation(call, continuation) {
        Ok(index) => index,
        Err(()) => {
            crate::trace_compiler!(
                "suspend",
                "append continuation BAIL: default operand plan has no ABI suffix call={call}"
            );
            return false;
        }
    };
    let continuation_position = match &mut ir.exprs[call as usize] {
        IrExpr::Call {
            args,
            callee: Callee::Static { descriptor, .. },
            ..
        } => {
            if let Some(planned_index) = planned_index {
                let Some((params, _)) = crate::jvm::ir_emit::parse_physical_method_desc(descriptor)
                else {
                    crate::trace_compiler!(
                        "suspend",
                        "append continuation BAIL: invalid default descriptor call={call} descriptor={descriptor}"
                    );
                    return false;
                };
                let Some(index) = default_suspend_continuation_index(&params) else {
                    crate::trace_compiler!(
                        "suspend",
                        "default suspend call={call} has no continuation slot descriptor={descriptor} params={params:?}"
                    );
                    return false;
                };
                if planned_index != index || index > args.len() {
                    crate::trace_compiler!(
                        "suspend",
                        "append continuation BAIL: operand-plan boundary mismatch call={call} planned={planned_index} descriptor={index} args={} descriptor_text={descriptor}",
                        args.len()
                    );
                    return false;
                }
                args.insert(index, continuation);
                index
            } else {
                *descriptor = cps_descriptor(descriptor);
                let index = args.len();
                args.push(continuation);
                index
            }
        }
        IrExpr::Call {
            args,
            callee:
                Callee::CrossFile {
                    params,
                    ret,
                    module_default_call,
                    ..
                },
            ..
        } => {
            let index = if *module_default_call {
                let Some(index) = planned_index else {
                    return false;
                };
                if index > args.len() || index > params.len() {
                    return false;
                }
                params.insert(index, continuation_ty());
                args.insert(index, continuation);
                index
            } else {
                if planned_index.is_some() {
                    return false;
                }
                params.push(continuation_ty());
                let index = args.len();
                args.push(continuation);
                index
            };
            *ret = object_ty();
            index
        }
        IrExpr::Call {
            args,
            callee: Callee::Virtual {
                descriptor, params, ..
            },
            ..
        } => {
            if planned_index.is_some() {
                return false;
            }
            if let Some((params, ret)) = params {
                params.push(continuation_ty());
                *ret = object_ty();
            } else {
                *descriptor = cps_descriptor(descriptor);
            }
            let index = args.len();
            args.push(continuation);
            index
        }
        IrExpr::Call {
            args,
            callee: Callee::LocalDefault(_) | Callee::ClassStaticDefault { .. },
            ..
        } => {
            let Some(index) = planned_index else {
                return false;
            };
            if index > args.len() {
                return false;
            }
            args.insert(index, continuation);
            index
        }
        IrExpr::Call { args, .. } => {
            if planned_index.is_some() {
                return false;
            }
            let index = args.len();
            args.push(continuation);
            index
        }
        IrExpr::MethodCall { args, .. } => {
            if planned_index.is_some() {
                return false;
            }
            let index = args.len();
            args.push(Some(continuation));
            index
        }
        IrExpr::InvokeFunction {
            args, params, ret, ..
        } => {
            if planned_index.is_some() {
                return false;
            }
            *ret = object_ty();
            let index = args.len();
            args.push(continuation);
            params.push(continuation_ty());
            index
        }
        _ => {
            return planned_index.is_none();
        }
    };
    operand_provenance.record_continuation(call, continuation_position)
}
