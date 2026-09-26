//! Suspension points of a function whose state machine kotlinc's coroutine transformer builds
//! from the finished method (`jvm::suspend::bytecode_machine`).
//!
//! Such a body is emitted as kotlinc's codegen emits it: each suspend call between
//! `InlineMarker.beforeInlineCall()` and `afterInlineCall()`, its invoke between `mark(0)` and
//! `mark(1)` (with `mark(11)` before a call whose result is `Unit`), and the call's erased `Object`
//! result coerced to the callee's declared result after the markers. The transformer reads the
//! markers, builds the machine and deletes them when the class is written.

use std::cell::RefCell;
use std::collections::HashMap;

use super::scalar_coercion::{semantic_scalar_adapter, unbox_prim_from};
use super::{debug_lines, jvm_declared_ty, EmitRun, Emitter};
use crate::ir::{ExprId, IrFile};
use crate::jvm::bytecode_passes::coroutines::markers::SuspendMarker;
use crate::jvm::classfile::{
    ClassWriter, CodeBuilder, CoroutineOutcome, CoroutineRequest, TransformedCoroutine,
};
use crate::jvm::suspend::{ContinuationMetadata, TransformedMachine};
use crate::types::Ty;

/// The suspension points of the function being emitted, by call, with each callee's declared
/// result. Empty for every function the transformer does not take.
pub(super) type TransformedSuspensions = HashMap<ExprId, Ty>;

impl Emitter<'_> {
    /// The declared result of `e` when it is a suspension point the transformer takes.
    pub(super) fn transformed_result(&self, e: ExprId) -> Option<Ty> {
        self.transformed_suspensions.get(&e).copied()
    }

    /// Mark where a call's own invoke starts: its line and, for a suspension point, the markers
    /// kotlinc's codegen writes right before the invoke, after the arguments.
    pub(super) fn mark_call_start(&mut self, e: ExprId, code: &mut CodeBuilder) {
        debug_lines::mark_expression_start(self.ir, e, code);
        let Some(result) = self.transformed_result(e) else {
            return;
        };
        code.suspend_marker(SuspendMarker::BeforeSuspend as i32, self.cw);
        if result == Ty::Unit {
            code.suspend_marker(SuspendMarker::BeforeSuspendUnitCall as i32, self.cw);
        }
    }

    /// Open suspension point `e`, before its arguments.
    pub(super) fn open_transformed_suspension(&mut self, e: ExprId, code: &mut CodeBuilder) {
        if self.transformed_suspensions.contains_key(&e) {
            code.inline_call_marker(true);
        }
    }

    /// Close suspension point `e` after its invoke, leaving the callee's declared result on the
    /// stack: nothing for `Unit`, and nothing at all when the result is `discarded`.
    pub(super) fn close_transformed_suspension(
        &mut self,
        e: ExprId,
        discarded: bool,
        code: &mut CodeBuilder,
    ) {
        let Some(result) = self.transformed_result(e) else {
            return;
        };
        code.suspend_marker(SuspendMarker::AfterSuspend as i32, self.cw);
        code.inline_call_marker(false);
        let target = jvm_declared_ty(&result);
        if discarded || target == Ty::Unit {
            code.pop();
        } else if target.is_jvm_scalar() {
            let object = Ty::obj("java/lang/Object");
            unbox_prim_from(
                self.cw,
                code,
                object,
                semantic_scalar_adapter(result, target),
            );
        } else {
            self.coerce_reference_on_stack(Ty::nullable(Ty::obj("kotlin/Any")), result, code);
        }
    }
}

impl Emitter<'_> {
    /// Arm the emission of a function the transformer takes: its suspension points, and the
    /// `$completion` parameter (value-index `completion`) they pass on, which a suspend lambda has
    /// none of. The completion's slot.
    pub(super) fn arm_transformed_machine(
        &mut self,
        machine: &TransformedMachine,
        completion: Option<u32>,
    ) -> u16 {
        let slot = completion.map_or(0, |completion| {
            let (slot, _) = *self
                .slots
                .get(&completion)
                .expect("a transformed suspend function retains its completion identity");
            self.continuation_slot = Some(slot);
            slot
        });
        self.transformed_suspensions = machine
            .suspensions
            .iter()
            .map(|suspension| (suspension.call, suspension.result))
            .collect();
        self.suspend_lambda_parameter_reads = machine
            .lambda
            .as_ref()
            .map(|lambda| lambda.parameter_reads.iter().copied().collect())
            .unwrap_or_default();
        slot
    }

    /// Mark the read of a suspend lambda's parameter from its field, just stored by declaration
    /// `e`, as kotlinc's codegen marks it: the coroutine starts after the last one.
    pub(super) fn mark_suspend_lambda_parameter_read(&mut self, e: ExprId, code: &mut CodeBuilder) {
        if self.suspend_lambda_parameter_reads.contains(&e) {
            code.suspend_marker(SuspendMarker::SuspendLambdaParameter as i32, self.cw);
        }
    }
}

/// Ask the class writer to transform the method just added for function `fid`.
pub(super) fn request_transform(
    ir: &IrFile,
    cw: &mut ClassWriter,
    fid: u32,
    method: (&str, &str),
    machine: &TransformedMachine,
    completion_slot: u16,
) {
    let (name, desc) = method;
    cw.request_coroutine_transform(
        name,
        desc,
        CoroutineRequest {
            continuation_class: machine.continuation_class.clone(),
            line_number: (*ir
                .fn_decl_lines
                .get(&fid)
                .expect("a transformed source function retains its declaration line"))
            .min(u16::MAX as u32) as u16,
            completion_slot,
            dispatch_receiver: dispatch_receiver(ir, fid).filter(|_| machine.lambda.is_none()),
            suspend_lambda: machine
                .lambda
                .as_ref()
                .map(|lambda| lambda.declared_spill_fields.clone()),
        },
    );
}

/// The class a member function's continuation captures as `this$0`, by internal name: a member's
/// own, or, for the `$suspendImpl` a member's body moved to, the receiver it takes first.
fn dispatch_receiver(ir: &IrFile, fid: u32) -> Option<String> {
    let function = &ir.functions[fid as usize];
    function
        .dispatch_receiver
        .filter(|_| !function.is_static)
        .or_else(|| {
            ir.jvm_suspend_impl_bodies
                .get(&fid)
                .map(|(owner, _)| *owner)
        })
        .map(|owner| owner.render())
}

/// What kotlinc's coroutine transformer found, by continuation class, for the classes written so
/// far. Their continuation classes are written after them.
pub(super) type TransformedCoroutines = RefCell<HashMap<String, CoroutineOutcome>>;

impl EmitRun {
    /// Write `class`, transforming the coroutines its methods asked for, and keep what they found.
    pub(super) fn finish_class(&self, class: ClassWriter) -> Vec<u8> {
        let (bytes, coroutines) = class.finish_with_coroutines();
        self.record_transformed_coroutines(coroutines);
        bytes
    }

    /// Keep what the transformation of a finished class found. A body it could not transform
    /// fails the compile: the class written holds no state machine.
    fn record_transformed_coroutines(&self, coroutines: Vec<TransformedCoroutine>) {
        for coroutine in coroutines {
            if let CoroutineOutcome::Failed(reason) = &coroutine.outcome {
                self.set_emit_error(format!("{}: {reason}", coroutine.continuation_class));
            }
            self.transformed_coroutines
                .borrow_mut()
                .insert(coroutine.continuation_class, coroutine.outcome);
        }
    }

    /// What the transformer found for the function continuation class `class` belongs to, when
    /// the transformer built its machine.
    pub(super) fn transformed_coroutine(&self, class: &str) -> Option<CoroutineOutcome> {
        self.transformed_coroutines.borrow().get(class).cloned()
    }
}

/// The continuation class's metadata once the transformer has built the machine: the arrays of
/// its `@DebugMetadata` are the transformer's.
pub(super) fn continuation_metadata(
    lowered: &ContinuationMetadata,
    outcome: &CoroutineOutcome,
) -> Option<ContinuationMetadata> {
    let CoroutineOutcome::StateMachine { debug_metadata, .. } = outcome else {
        return None;
    };
    Some(ContinuationMetadata {
        l: debug_metadata.line_numbers.clone(),
        nl: debug_metadata.next_line_numbers.clone(),
        i: debug_metadata.index_to_label.clone(),
        s: debug_metadata.spilled.clone(),
        n: debug_metadata.local_names.clone(),
        m: debug_metadata.method_name.clone(),
        c: debug_metadata.class_name.clone(),
        v: debug_metadata.version,
        ..lowered.clone()
    })
}

/// Declare the spill fields the transformer's machine stores into, ahead of `result` and `label`
/// as kotlinc declares them.
pub(super) fn add_spill_fields(cw: &mut ClassWriter, outcome: Option<&CoroutineOutcome>) {
    let Some(CoroutineOutcome::StateMachine { fields, .. }) = outcome else {
        return;
    };
    for field in fields {
        cw.add_field_sig(0, &field.name, &field.descriptor, None);
    }
}
