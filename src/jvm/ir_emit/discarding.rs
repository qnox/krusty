//! Statement-position JVM emission and operand disposal.
//!
//! Common IR keeps expression values even where Kotlin source discards them. This boundary chooses
//! the JVM statement shape, including control flow whose branches must discard independently.

use super::{bottom_values, discard, CodeBuilder, Emitter, IrExpr};

impl Emitter<'_> {
    pub(super) fn emit_discarding(&mut self, expression: u32, code: &mut CodeBuilder) {
        let node = self.ir.expr(expression).clone();
        self.emit_discarding_node(expression, &node, code);
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
                // kotlinc never materializes a discarded `Unit`.
                IrExpr::UnitInstance => return,
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
                _ => {}
            }
        }
        let suspension = self.machine_before(expression, code);
        self.open_transformed_suspension(expression, code);
        self.emit_value_node(expression, node, code);
        self.machine_after(suspension, code);
        if self.transformed_result(expression).is_some() {
            // kotlinc discards the erased result itself, without coercing it first.
            self.close_transformed_suspension(expression, true, code);
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
