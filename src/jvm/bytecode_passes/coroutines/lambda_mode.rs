//! `CoroutineTransformerMethodVisitor.performTransformations` for a suspend lambda's
//! `invokeSuspend` (`isForNamedFunction = false`).
//!
//! The lambda is its own continuation: the machine keeps `label` and its spill fields in the lambda
//! class, `this` (slot 0) is the continuation and the `$result` parameter (slot 1) the resumption
//! result, so there is no prelude that finds or creates a continuation, and no tail-call form. The
//! lambda's parameters were stored in spill-named fields of the class before `invokeSuspend` runs;
//! codegen reads each back at the top of the body and marks it with `mark(10)`. That reading runs
//! on every entry, so the `tableswitch` goes after it, a parameter is spilled but never restored,
//! and the fields the class already declares are not declared again.
//!
//! kotlinc's `markFakeLineNumberForLambdaArgumentUnspilling` only adds a line under
//! `enhancedCoroutinesDebugging`, which this port does not model, as the named-function mode does
//! not model it either.

use super::super::insn_list::EditableMethod;
use super::markers::{is_suspend_marker, SuspendMarker};
use super::state_machine::Machine;
use super::transform::{build_state_machine, prepare_marked_body, MachineSpec};
use super::{CoroutineError, DeclaredSpillFields, StateMachine};
use crate::jvm::method_node::MethodNode;

/// The suspend lambda whose `invokeSuspend` is being transformed (kotlinc's
/// `isForNamedFunction = false`): the lambda is its own continuation, so the machine keeps its
/// state in the lambda class.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SuspendLambda<'a> {
    /// The lambda class's internal name (`AKt$f$1`), which declares `invokeSuspend`.
    pub class: &'a str,
    pub source_file: &'a str,
    /// The lambda's first line: the machine's own code is attributed to it.
    pub line_number: u16,
    /// The spill fields the lambda class already declares for its parameters, which the machine
    /// reuses rather than declares again (`initialVarsCountByType`), in the class's order.
    pub declared_spill_fields: &'a [DeclaredSpillFields<'a>],
}

/// `invokeSuspend`'s own descriptor.
const INVOKE_SUSPEND_DESCRIPTOR: &str = "(Ljava/lang/Object;)Ljava/lang/Object;";
/// The lambda itself (`continuationIndex`).
const CONTINUATION_SLOT: u16 = 0;
/// The `$result` parameter (`dataIndex`), which is also `getLastParameterIndex`.
const RESULT_SLOT: u16 = 1;

/// Transforms the marked `invokeSuspend` body of a suspend lambda the way kotlinc's
/// `CoroutineTransformerMethodVisitor` does.
pub(crate) fn transform_suspend_lambda(
    method: MethodNode,
    lambda: &SuspendLambda,
) -> Result<StateMachine, CoroutineError> {
    if method.access & 0x0008 != 0
        || method.desc != INVOKE_SUSPEND_DESCRIPTOR
        || method.max_locals <= RESULT_SLOT
    {
        return Err(CoroutineError::Unsupported(
            "a suspend lambda body that is not an instance `invokeSuspend(Object): Object`",
        ));
    }
    let owner = lambda.class;
    let mut method = EditableMethod::new(method);
    let points = prepare_marked_body(&mut method, owner, CONTINUATION_SLOT)?;

    // `actualCoroutineStart`: past the parameters' reading, when there is any.
    let last_parameter_marker =
        method.insns.ids().into_iter().rev().find(|&id| {
            is_suspend_marker(&method.insns, id, SuspendMarker::SuspendLambdaParameter)
        });
    let coroutine_start = match last_parameter_marker {
        Some(marker) => method
            .insns
            .next(marker)
            .ok_or(CoroutineError::Unsupported(
                "a suspend lambda parameter marker that ends the body",
            ))?,
        None => method
            .insns
            .first()
            .ok_or(CoroutineError::Unsupported("an empty suspend lambda body"))?,
    };

    let spec = MachineSpec {
        owner,
        source_file: lambda.source_file,
        machine: Machine {
            state_class: lambda.class,
            line_number: lambda.line_number,
            continuation_index: CONTINUATION_SLOT,
            data_index: RESULT_SLOT,
        },
        completion_slot: None,
        last_parameter_slot: RESULT_SLOT,
        declared_spill_fields: lambda.declared_spill_fields,
    };
    build_state_machine(method, points, &spec, coroutine_start)
}
