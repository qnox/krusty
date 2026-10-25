//! Suspension points of a function whose state machine kotlinc's coroutine transformer builds
//! from the finished method (`jvm::suspend::bytecode_machine`).
//!
//! Such a body is emitted as kotlinc's codegen emits it: each suspend call between
//! `InlineMarker.beforeInlineCall()` and `afterInlineCall()`, its invoke between `mark(0)` and
//! `mark(1)` (with `mark(11)` before a call whose result is `Unit`), and the call's erased `Object`
//! result coerced to the callee's declared result after the markers. A call whose resumption
//! delivers the box of the value-class carrier it returns unboxes it right after `mark(1)`,
//! between `mark(8)` and `mark(9)`: the transformer moves that unboxing to the resume path. The transformer reads the
//! markers, builds the machine and deletes them when the class is written.

use crate::fx_hash::{HashMap, HashSet};
use std::cell::RefCell;

use super::scalar_coercion::{semantic_scalar_adapter, unbox_prim_from};
use super::{jvm_declared_ty, EmitRun, Emitter};
use crate::ir::{ExprId, IrExpr, IrFile, IrTypeOp};
use crate::jvm::bytecode_passes::coroutines::markers::SuspendMarker;
use crate::jvm::classfile::{
    ClassWriter, CodeBuilder, CoroutineOutcome, CoroutineRequest, TransformedCoroutine,
};
use crate::jvm::suspend::{ContinuationMetadata, TransformedMachine};
use crate::types::{Ty, TypeName};

/// The suspension points of the function being emitted, by call, with each callee's declared
/// result, and the continuation reads written as fake continuations. Empty for every function the
/// transformer does not take.
#[derive(Default)]
pub(super) struct TransformedSuspensions {
    results: HashMap<ExprId, Ty>,
    /// The value class and carrier of each point whose resumption delivers the box.
    resumed_boxes: HashMap<ExprId, (TypeName, Ty)>,
    fake_continuations: HashSet<ExprId>,
    /// Points a body kept for inlining writes without markers: invokes of an inline lambda
    /// parameter, which the inlining caller replaces with the lambda's body.
    inlined: HashSet<ExprId>,
    /// The suspension point a local declaration's initializer is, while that initializer is
    /// emitted: kotlinc's `visitVariable` marks the initializer's line before casting its value.
    initializer: Option<ExprId>,
    /// The body is a suspend lambda's placed one, whose calls pass the bare fake continuation.
    placed_lambda: bool,
}

/// What a suspension point leaves on the stack after its invoke.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SuspensionResult {
    /// The callee's declared result.
    Declared,
    /// Nothing: the result is discarded.
    Discarded,
    /// The erased `Object` result as the invoke left it.
    Erased,
}

impl Emitter<'_> {
    /// The declared result of `e` when it is a suspension point the transformer takes.
    /// Whether kotlinc's coroutine transformer builds this method's machine: its suspension
    /// markers are then the transformer's stand-in instructions.
    pub(super) fn transformer_owns_method(&self) -> bool {
        !self.transformed_suspensions.results.is_empty()
    }

    pub(super) fn transformed_result(&self, e: ExprId) -> Option<Ty> {
        self.transformed_suspensions.results.get(&e).copied()
    }

    /// Write continuation read `e` as kotlinc's fake continuation, when it is one: the transformer
    /// replaces it with the machine's continuation. `false`, having written nothing, otherwise.
    pub(super) fn emit_fake_continuation(&mut self, e: ExprId, code: &mut CodeBuilder) -> bool {
        if !self.is_fake_continuation(e) {
            return false;
        }
        if self.transformed_suspensions.placed_lambda {
            // `addFakeContinuationMarker`: the call takes the `null` as its continuation.
            code.suspend_marker(SuspendMarker::FakeContinuation as i32, self.cw);
            code.aconst_null();
        } else {
            self.load_fake_continuation(code);
        }
        true
    }

    /// Whether `e` reads kotlinc's fake continuation rather than the function's own.
    pub(super) fn is_fake_continuation(&self, e: ExprId) -> bool {
        self.transformed_suspensions.fake_continuations.contains(&e)
    }

    /// kotlinc's fake continuation, cast to the interface the block's parameter and the probe take.
    pub(super) fn load_fake_continuation(&mut self, code: &mut CodeBuilder) {
        code.suspend_marker(SuspendMarker::FakeContinuation as i32, self.cw);
        code.aconst_null();
        let continuation = self.cw.class_ref("kotlin/coroutines/Continuation");
        code.checkcast(continuation);
    }

    /// Note that `initializer`, a local declaration's, is about to be emitted, returning the
    /// initializer noted before it for [`Self::leave_declared_initializer`]. A coercion to a
    /// reference type or a statement-less block around a suspension point is that point.
    pub(super) fn enter_declared_initializer(&mut self, initializer: ExprId) -> Option<ExprId> {
        let mut e = initializer;
        loop {
            e = match self.ir.expr(e) {
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } => *arg,
                IrExpr::Block {
                    stmts,
                    value: Some(value),
                } if stmts.is_empty() => *value,
                _ => break,
            };
        }
        let point = self.transformed_result(e).map(|_| e);
        std::mem::replace(&mut self.transformed_suspensions.initializer, point)
    }

    /// Restore the initializer noted before [`Self::enter_declared_initializer`].
    pub(super) fn leave_declared_initializer(&mut self, previous: Option<ExprId>) {
        self.transformed_suspensions.initializer = previous;
    }

    /// Whether `e` is a `suspendCoroutineUninterceptedOrReturn` block the transformer takes, whose
    /// probe reads the fake continuation.
    pub(super) fn is_transformed_block(&self, e: ExprId) -> bool {
        self.transformed_result(e).is_some() && self.ir.is_unintercepted_suspension(e)
    }

    /// Emit `e`, a value coerced to `target`, leaving the erased `Object` result when it is a
    /// suspension point and `target` is `Object`: kotlinc coerces a suspension point's result only
    /// for a consumer that needs a narrower type, so `return step()` returns the result as is.
    /// `false`, having emitted nothing, for anything else.
    pub(super) fn emit_erased_suspension_result(
        &mut self,
        e: ExprId,
        target: Ty,
        code: &mut CodeBuilder,
    ) -> bool {
        if target != Ty::obj("java/lang/Object") {
            return false;
        }
        // An implicit coercion to a reference type between them narrows nothing kotlinc writes, nor
        // does the one narrowing a dependency's erased result to its declared type.
        // A statement-less block around the point, like the one a value-class tail that already
        // is the callee's box leaves, is the point itself.
        let mut e = e;
        loop {
            e = match self.ir.expr(e) {
                IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg,
                    type_operand,
                } if super::ir_ty_to_jvm(type_operand).is_reference()
                    || self.transformed_result(*arg).is_some() =>
                {
                    *arg
                }
                IrExpr::Block {
                    stmts,
                    value: Some(value),
                } if stmts.is_empty() => *value,
                _ => break,
            };
        }
        if self
            .transformed_result(e)
            .is_none_or(|result| result == Ty::Unit)
        {
            return false;
        }
        self.mark_expression_start(e, code);
        self.open_transformed_suspension(e, code);
        let node = self.ir.expr(e).clone();
        self.emit_value_node(e, &node, code);
        self.mark_after_inlined_call(e, code);
        self.probe_intrinsic_suspension(e, code);
        self.close_transformed_suspension(e, SuspensionResult::Erased, code);
        true
    }

    /// Mark where a call's own invoke starts: its line and, for a suspension point, the markers
    /// kotlinc's codegen writes right before the invoke, after the arguments.
    pub(super) fn mark_call_start(&mut self, e: ExprId, code: &mut CodeBuilder) {
        self.mark_dispatch_line(e, code);
        let Some(result) = self.transformed_result(e).filter(|_| self.is_marked(e)) else {
            return;
        };
        code.suspend_marker(SuspendMarker::BeforeSuspend as i32, self.cw);
        if result == Ty::Unit {
            code.suspend_marker(SuspendMarker::BeforeSuspendUnitCall as i32, self.cw);
        }
    }

    /// Open suspension point `e`, before its arguments. A block has no invoke to mark: it is the
    /// point from its first instruction.
    pub(super) fn open_transformed_suspension(&mut self, e: ExprId, code: &mut CodeBuilder) {
        if self.transformed_suspensions.results.contains_key(&e) && self.is_marked(e) {
            code.inline_call_marker(true);
        }
        if self.is_transformed_block(e) {
            code.suspend_marker(SuspendMarker::BeforeSuspend as i32, self.cw);
            if self.transformed_result(e) == Some(Ty::Unit) {
                code.suspend_marker(SuspendMarker::BeforeSuspendUnitCall as i32, self.cw);
            }
        }
    }

    /// Close suspension point `e` after its invoke, leaving the callee's declared result.
    pub(super) fn close_declared_suspension(&mut self, e: ExprId, code: &mut CodeBuilder) {
        self.close_transformed_suspension(e, SuspensionResult::Declared, code);
    }

    /// Close suspension point `e` after its invoke, leaving what `result` asks for of the callee's
    /// erased `Object` result.
    pub(super) fn close_transformed_suspension(
        &mut self,
        e: ExprId,
        leave: SuspensionResult,
        code: &mut CodeBuilder,
    ) {
        let Some(result) = self.transformed_result(e) else {
            return;
        };
        if self.is_marked(e) {
            code.suspend_marker(SuspendMarker::AfterSuspend as i32, self.cw);
            if let Some(&(value_class, carrier)) =
                self.transformed_suspensions.resumed_boxes.get(&e)
            {
                self.unbox_on_resume(value_class, carrier, code);
            }
            code.inline_call_marker(false);
        }
        let block = self.is_transformed_block(e);
        if block {
            // The block was an inlined body: the line is written afresh after it.
            code.forget_line();
        }
        if leave == SuspensionResult::Erased {
            return;
        }
        let discarded = leave == SuspensionResult::Discarded;
        // A used result is read as the declared type at the call's own line after the block. A
        // declaration's initializer is too once an inline suspend call's body left the caller's
        // line forgotten: kotlinc's `visitVariable` marks it before the cast. Anywhere else the
        // next line mark is the consumer's own.
        let initializer = self.transformed_suspensions.initializer == Some(e);
        let marked = block || (initializer && code.line_forgotten());
        match self.ir.expr_source_lines.get(&e) {
            Some(&line) if marked && !discarded && line != 0 => code.mark_line(line),
            _ => {}
        }
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
    /// kotlinc's `generateResumePathUnboxing`: the box's unboxing to `carrier`, then a cast to
    /// `Object` that keeps the direct path's type, between the markers. The transformer copies
    /// what precedes the cast to the resume path and deletes the rest.
    fn unbox_on_resume(&mut self, value_class: TypeName, carrier: Ty, code: &mut CodeBuilder) {
        code.suspend_marker(SuspendMarker::BeforeUnboxInlineClass as i32, self.cw);
        super::value_class_adapters::emit_value_class_unbox_adapter(
            self.cw,
            code,
            value_class,
            super::ir_ty_to_jvm(&carrier).non_null(),
            crate::jvm::suspend::box_may_be_null(self.ir, value_class, carrier),
        );
        let object = self.cw.class_ref("java/lang/Object");
        code.checkcast(object);
        code.suspend_marker(SuspendMarker::AfterUnboxInlineClass as i32, self.cw);
    }

    /// Arm the emission of a function the transformer takes: its suspension points, and the
    /// `$completion` parameter (value-index `completion`) they pass on, which a suspend lambda has
    /// none of. The completion's slot.
    pub(super) fn arm_transformed_machine(
        &mut self,
        machine: &TransformedMachine,
        completion: Option<u32>,
    ) -> u16 {
        // A suspend lambda is its own continuation, in slot 0: a lambda the method inliner places
        // in its body captures it from there.
        let slot = completion.map_or(0, |completion| {
            let (slot, _) = *self
                .slots
                .get(&completion)
                .expect("a transformed suspend function retains its completion identity");
            slot
        });
        self.continuation_slot = Some(slot);
        self.transformed_suspensions = TransformedSuspensions {
            results: machine
                .suspensions
                .iter()
                .map(|suspension| (suspension.call, suspension.result))
                .collect(),
            resumed_boxes: machine
                .suspensions
                .iter()
                .filter_map(|suspension| Some((suspension.call, suspension.resumed_box?)))
                .collect(),
            fake_continuations: machine.fake_continuations.iter().copied().collect(),
            inlined: HashSet::default(),
            initializer: None,
            placed_lambda: false,
        };
        self.suspend_lambda_parameter_reads = machine
            .lambda
            .as_ref()
            .map(|lambda| lambda.parameter_reads.iter().copied().collect())
            .unwrap_or_default();
        slot
    }

    /// Arm the emission of a suspend lambda's placed body: its points are `machine`'s, each passing
    /// a fake continuation, and the lambda's `$completion` is in `slot`. The state to restore once
    /// the body is emitted.
    pub(super) fn arm_placed_suspend_lambda(
        &mut self,
        machine: &TransformedMachine,
        slot: u16,
    ) -> (TransformedSuspensions, Option<u16>) {
        let continuation = self.continuation_slot.replace(slot);
        let suspensions = std::mem::replace(
            &mut self.transformed_suspensions,
            TransformedSuspensions {
                results: machine
                    .suspensions
                    .iter()
                    .map(|suspension| (suspension.call, suspension.result))
                    .collect(),
                resumed_boxes: machine
                    .suspensions
                    .iter()
                    .filter_map(|suspension| Some((suspension.call, suspension.resumed_box?)))
                    .collect(),
                fake_continuations: machine.fake_continuations.iter().copied().collect(),
                placed_lambda: true,
                ..TransformedSuspensions::default()
            },
        );
        (suspensions, continuation)
    }

    /// Restore what [`Self::arm_placed_suspend_lambda`] returned.
    pub(super) fn disarm_placed_suspend_lambda(
        &mut self,
        (suspensions, continuation): (TransformedSuspensions, Option<u16>),
    ) {
        self.transformed_suspensions = suspensions;
        self.continuation_slot = continuation;
    }

    /// Leave `calls`, points of the machine just armed, unmarked: the body is kept for inlining
    /// and they are invokes of inline lambda parameters.
    pub(super) fn leave_inlined_invokes_unmarked(&mut self, calls: Vec<ExprId>) {
        self.transformed_suspensions.inlined = calls.into_iter().collect();
    }

    /// Whether suspension point `e` is written between its markers.
    fn is_marked(&self, e: ExprId) -> bool {
        !self.transformed_suspensions.inlined.contains(&e)
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
            // kotlinc's transformer opens the machine on the declaration's own line, which an
            // expression body on a later line does not move.
            line_number: ir
                .fn_sig_lines
                .get(&fid)
                .or_else(|| ir.fn_decl_lines.get(&fid))
                .map(|&line| line.min(u16::MAX as u32) as u16),
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
/// own, or, for the `$suspendImpl` a member's body moved to, the receiver it takes first. A named
/// object's `@JvmStatic` member is static and captures none.
fn dispatch_receiver(ir: &IrFile, fid: u32) -> Option<String> {
    let function = &ir.functions[fid as usize];
    let jvm_static = super::jvm_static_members::singleton_static_owner(ir, fid).is_some();
    function
        .dispatch_receiver
        .filter(|_| !function.is_static && !jvm_static)
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
    /// A method whose saved operand stack cannot be normalized fails the compile.
    pub(super) fn finish_class(&self, mut class: ClassWriter) -> Vec<u8> {
        // A failed method can stop before restoring a valid operand stack. The whole emit pass is
        // discarded once this flag is observed, so do not ask the frame computer to analyze that
        // incomplete class in the meantime. These bytes cannot escape `emit_pass`.
        if self.emission_failed() {
            return Vec::new();
        }
        // A class whose code switches on an enum declares its `$WhenMappings` nested class.
        if let Some(mappings) = self
            .when_mappings
            .borrow()
            .inner_class(&class.internal_name)
        {
            class.add_inner_class(mappings);
        }
        // A class that inlines a reified `enumEntries` of an enum without `getEntries()` declares
        // its `$EntriesIntrinsicMappings` nested class.
        if let Some(mappings) = self
            .entries_mappings
            .borrow()
            .inner_class(&class.internal_name)
        {
            class.add_inner_class(mappings);
        }
        let finished = class.finish_with_coroutines();
        for failure in finished.failures {
            self.set_emit_error(failure);
        }
        self.record_transformed_coroutines(finished.coroutines);
        finished.bytes
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

/// The continuations of functions with no method of their own, which nothing constructs: a
/// suspend lambda's every call places its body.
pub(super) fn unwritten_continuations(ir: &IrFile, env: &super::EmitEnv) -> HashSet<TypeName> {
    (0..ir.functions.len() as u32)
        .filter(|&function| super::standalone_method_is_elided(ir, function, env))
        .filter_map(|function| env.emit_time_machines.transformed(function))
        .map(|machine| crate::types::type_name(&machine.continuation_class))
        .collect()
}
