//! Statement-position JVM emission and operand disposal.
//!
//! Common IR keeps expression values even where Kotlin source discards them. This boundary chooses
//! the JVM statement shape, including control flow whose branches must discard independently.

use super::{bottom_values, slot_words, CodeBuilder, Emitter, IrExpr, Ty};

impl Emitter<'_> {
    fn discards_copied_frame_result(&self, expression: u32) -> bool {
        matches!(
            self.ir.expr(expression),
            IrExpr::Block {
                stmts,
                value: Some(value),
            } if self
                .discarded_copied_frame_result(expression, stmts, *value)
                .is_some()
        )
    }

    pub(super) fn emit_discarding(&mut self, expression: u32, code: &mut CodeBuilder) {
        let node = self.ir.expr(expression).clone();
        self.emitting(expression, |emitter| {
            emitter.emit_discarding_node(expression, &node, code);
        });
    }

    pub(super) fn emit_discarding_node(
        &mut self,
        expression: u32,
        node: &IrExpr,
        code: &mut CodeBuilder,
    ) {
        if self.emit_discarded_safe_call(expression, node, code) {
            return;
        }
        // An in-place delegate evaluated where its field was initialized keeps its property's
        // line even when the evaluation leaves no instruction (`Unit`): the next line's mark gives
        // it a `nop` of its own.
        if self.ir.is_jvm_delegate_evaluation(expression) {
            if let Some(&line) = self.ir.expr_source_lines.get(&expression) {
                code.mark_line_occupied(line);
            }
        }
        if let IrExpr::BottomValue {
            producer,
            completion,
        } = node
        {
            let baseline = code.stack_height();
            self.emit_value(*producer, code);
            bottom_values::finish(
                self.cw,
                code,
                baseline,
                completion.diverges_when_discarded(),
            );
            return;
        }
        // A discarded `when` is a statement: its branches discard their own values, so no branch
        // leaves a value another branch does not. A block ending in one runs as a statement block,
        // which discards that `when` the same way. A discarded `Unit` is nothing at all.
        if !self.machine_suspensions.contains_key(&expression) {
            match node {
                // A copied inline body's terminal `Unit` carries its mapped closing line on a
                // `nop`, even though the value itself is not materialized.
                IrExpr::UnitInstance => {
                    if self.has_retained_mapped_inline_unit_line(expression) {
                        self.mark_expression_start(expression, code);
                        code.nop();
                    }
                    return;
                }
                // A generic inline return specialized to Unit already stored
                // `kotlin.Unit.INSTANCE` before `finally`. The caller that discards the call
                // does not reload that slot.
                IrExpr::GetValue(index)
                    if self.inline_return_frame_result_type(*index)
                        == Some(Ty::obj("kotlin/Unit")) =>
                {
                    return;
                }
                IrExpr::When { branches } => {
                    self.emit_when(expression, branches, true, code);
                    return;
                }
                // A discarded block is a statement block: its trailing read is not a value the
                // break out of an inline-return frame has to carry.
                IrExpr::Block { stmts, value } if !self.is_transformed_block(expression) => {
                    self.emit_statement_block(expression, stmts.clone(), *value, code);
                    return;
                }
                // A discarded `try` runs its branches as statements, as a discarded `when` does,
                // but still holds the result temporary its type gives it.
                IrExpr::Try {
                    body,
                    catches,
                    finally,
                    result,
                } => {
                    let parts = super::try_emission::TryParts {
                        body: *body,
                        catches,
                        finally: *finally,
                        result: *result,
                    };
                    self.emit_try(expression, parts, true, code);
                    return;
                }
                IrExpr::Block {
                    value: Some(value), ..
                } if matches!(self.ir.expr(*value), IrExpr::When { .. }) => {
                    self.emit(expression, code);
                    return;
                }
                // A copied inline frame owns the trailing value until its locals end. Run that
                // frame as a statement so its emitter can pop the physical result after the
                // boundary, without the coercion a consumer would need.
                IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } if self.discards_copied_frame_result(*arg) => {
                    let IrExpr::Block { stmts, value } = self.ir.expr(*arg) else {
                        unreachable!("a copied result boundary is a block");
                    };
                    let (stmts, value) = (stmts.clone(), *value);
                    self.emit_statement_block(*arg, stmts, value, code);
                    return;
                }
                // A property assignment is `Unit` even when the Java setter returns the receiver
                // or another value. That coercion is also a declaration-result coercion, and it
                // carries no physical type: `value_ty` names the language result (`kotlin/Long`
                // for a `long` setter) while the invoke leaves the descriptor's return. Drop
                // whatever the stack tracker says the producer pushed. A void call leaves the
                // height unchanged and is not popped.
                IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg,
                    type_operand,
                } if *type_operand == crate::types::Ty::Unit && !self.diverges(*arg) => {
                    // A carried inline result is already on the stack at the landing. Reading it
                    // consumes that resident value and does not change the tracked height, so the
                    // pop is the landing itself: the load stayed before the jump.
                    let carried = matches!(
                        self.ir.expr(*arg),
                        IrExpr::GetValue(index) if self.stack_resident_value == Some(*index)
                    );
                    let before = code.stack_height();
                    self.emit_value(*arg, code);
                    if carried {
                        code.pop();
                    } else {
                        match code.stack_height() - before {
                            1 => code.pop(),
                            2 => code.pop2(),
                            _ => {}
                        }
                    }
                    return;
                }
                // A generic call's erased result is discarded as it is. Reading it as the
                // substituted type (a `checkcast`, an unboxing) exists only for a consumer, and
                // kotlinc pops the erased value. A dependency member's slot is the one its
                // descriptor returns, so the call's own stack effect names what to pop.
                IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } if self.is_call_result_boundary(expression, *arg) && !self.diverges(*arg) => {
                    // A suspension point the coroutine transformer closes discards its erased
                    // result at the resumption join, as a discarded suspending call does.
                    if self.transformed_result(*arg).is_some() {
                        self.emit_discarding(*arg, code);
                        return;
                    }
                    let before = code.stack_height();
                    self.emit_value(*arg, code);
                    match code.stack_height() - before {
                        2 => code.pop2(),
                        1 => code.pop(),
                        _ => {}
                    }
                    return;
                }
                // The unnamed implicit check reads its temporary and consumes it: there is no
                // duplicate to discard (`aload; invokestatic checkNotNull`).
                IrExpr::NotNullAssert {
                    operand,
                    check: crate::ir::NullCheck::Unnamed,
                } => {
                    self.emit_value(*operand, code);
                    let check = self.cw.methodref(
                        "kotlin/jvm/internal/Intrinsics",
                        "checkNotNull",
                        "(Ljava/lang/Object;)V",
                    );
                    code.invokestatic(check, 1, 0);
                    return;
                }
                _ => {}
            }
        }
        if self.emit_discarded_boxed_call(expression, node, code) {
            return;
        }
        // `FunctionN.invoke` physically returns `Object`. Narrowing that result to the function's
        // declared type exists only for a consumer; a statement-position invocation discards the
        // erased slot directly, as a discarded generic call does above. Suspension points keep
        // their existing materialization path because the coroutine machine owns their join type.
        let discarded_erased_invocation = matches!(
            node,
            IrExpr::InvokeFunction { ret, .. }
                if !matches!(*ret, crate::types::Ty::Unit | crate::types::Ty::Nothing)
                    && !self.machine_suspensions.contains_key(&expression)
                    && self.transformed_result(expression).is_none()
        );
        if discarded_erased_invocation {
            self.erased_invocations.insert(expression);
        }
        let suspension = self.machine_before(expression, code);
        self.open_transformed_suspension(expression, code);
        self.emit_value_node(expression, node, code);
        // A discarded root that ends an inlined call forgets the caller's line like a used one.
        self.mark_after_inlined_call(expression, code);
        // `emit_value_node` normally consumes this marker at the invocation. Remove it defensively
        // for aliases whose emission bypasses the ordinary `InvokeFunction` result path.
        self.erased_invocations.remove(&expression);
        // A statement still suspends. The probe reads the result before it is popped, the same
        // way a used result is probed before its consumer.
        self.probe_intrinsic_suspension(expression, code);
        self.machine_after(suspension, code);
        if self.transformed_result(expression).is_some() {
            // kotlinc discards the erased result itself, without coercing it first.
            self.close_transformed_suspension(
                expression,
                super::transformed_suspensions::SuspensionResult::Discarded,
                code,
            );
            return;
        }
        // A successfully spliced bottom-typed expression has already transferred control (for
        // example, an inline lambda's non-local `return`). It leaves no value to discard. The
        // semantic type is retained on the IR expression even when the selected callable's physical
        // descriptor returns `Object`.
        if self.diverges(expression) {
            return;
        }
        discard(
            if discarded_erased_invocation {
                crate::types::Ty::obj("java/lang/Object")
            } else {
                self.value_ty(expression)
            },
            code,
        );
    }
}

/// Pop a value of type `t` (two words for `long`/`double`, none for `void`).
pub(super) fn discard(t: Ty, code: &mut CodeBuilder) {
    match slot_words(t) {
        2 => code.pop2(),
        1 => code.pop(),
        _ => {}
    }
}
