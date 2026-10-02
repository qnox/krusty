//! Statement-position JVM emission and operand disposal.
//!
//! Common IR keeps expression values even where Kotlin source discards them. This boundary chooses
//! the JVM statement shape, including control flow whose branches must discard independently.

use super::{bottom_values, discard, CodeBuilder, Emitter, IrExpr};

impl Emitter<'_> {
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
        if !self.machine_suspensions.contains(&expression) {
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
                IrExpr::When { branches } => {
                    self.emit_when(expression, branches, true, code);
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
                    let before = code.stack_height();
                    self.emit_value(*arg, code);
                    match code.stack_height() - before {
                        1 => code.pop(),
                        2 => code.pop2(),
                        _ => {}
                    }
                    return;
                }
                // A generic call's erased result is discarded as it is. Reading it as the
                // substituted type (a `checkcast`, an unboxing) exists only for a consumer, and
                // kotlinc pops the erased value.
                IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::ImplicitCoercion,
                    arg,
                    ..
                } if self.ir.declaration_result_coercions.contains(&expression) => {
                    if let Some(&erased) = self.ir.physical_types.get(arg) {
                        self.emit_value(*arg, code);
                        discard(super::ir_ty_to_jvm(&erased), code);
                        return;
                    }
                }
                _ => {}
            }
        }
        if self.emit_discarded_boxed_call(expression, node, code) {
            return;
        }
        let suspension = self.machine_before(expression, code);
        self.open_transformed_suspension(expression, code);
        self.emit_value_node(expression, node, code);
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
        discard(self.value_ty(expression), code);
    }
}
