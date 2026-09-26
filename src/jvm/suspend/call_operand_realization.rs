//! JVM CPS operand realization for calls selected before suspend lowering.
//!
//! This boundary owns the physical continuation slot and publishes its exact position to emission.
//! Neither the emitter nor later transforms recover that role from a type, name, or operand shape.

use super::{continuation_ty, object_ty};
use crate::ir::{Callee, ExprId, IrExpr, IrFile};

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
                // The default-operand plan owns this ABI boundary. The descriptor is checked only
                // for consistency with that recorded role; it never rediscovers the role by name.
                if params.len() != args.len() + 1
                    || params.get(planned_index).copied() != Some(continuation_ty())
                    || planned_index > args.len()
                {
                    crate::trace_compiler!(
                        "suspend",
                        "append continuation BAIL: operand-plan/descriptor mismatch call={call} planned={planned_index} args={} descriptor_text={descriptor} params={params:?}",
                        args.len()
                    );
                    return false;
                }
                args.insert(planned_index, continuation);
                planned_index
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
            callee: Callee::Special { descriptor, .. },
            ..
        } => {
            if planned_index.is_some() {
                return false;
            }
            *descriptor = cps_descriptor(descriptor);
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
