//! Assignment to a captured mutable local's shared-cell storage.
//!
//! This owns the operand spill, shared-cell store, and source-line restoration as one operation:
//! copied inline values can temporarily enter a nested source frame while the assignment itself
//! remains on the captured local's source line.

use super::super::*;

pub(in crate::jvm::ir_emit) fn emit(
    emitter: &mut Emitter<'_>,
    expression: ExprId,
    holder: ExprId,
    elem: Ty,
    value: ExprId,
    code: &mut CodeBuilder,
) {
    // Like a plain local assignment, a captured-local assignment starts on its own source line
    // before evaluating the storage receiver. This also lets a copied lambda map the receiver load
    // through its inline source frame.
    emitter.mark_expression_start(expression, code);
    let enters_nested_frame =
        debug_lines::enters_nested_copied_lambda_frame(emitter.ir, expression, value);

    // A value that cannot carry the operand stack (`msg = try {…} finally {}`) can't run with the
    // holder on it. Spill it first, as shared-cell declaration does.
    let unsigned = emitter.completes_unsigned_bitwise_value(value);
    let saved_assignment = emitter.unsigned_assignment_line;
    if unsigned {
        // A local assignment keeps the line on the receiver and writes it again at the store. A
        // captured local is the same assignment once the holder is gone.
        emitter.unsigned_assignment_line = true;
    }
    if emitter.spills_operand_prefix(value) {
        let temps = emitter.spill_to_temps(&[value], code);
        emitter.emit_value(holder, code);
        for &(slot, ty, _) in &temps {
            load(ty, slot, code);
        }
        emitter.release_operand_spills(&temps);
    } else {
        emitter.emit_value(holder, code);
        emitter.emit_element_value(elem, value, code);
    }
    emitter.unsigned_assignment_line = saved_assignment;

    if unsigned {
        emitter.mark_expression_start(expression, code);
        code.forget_line();
        debug_lines::mark_statement(emitter.ir, expression, code);
    }
    if emitter.ir.expr_source_lines.contains_key(&value) || enters_nested_frame {
        if let Some(&statement_line) = emitter.ir.expr_lines.get(&expression) {
            emitter.mark_expression_line(expression, statement_line, code);
        }
    }

    let (class, descriptor) = ref_class(&elem);
    let field = emitter.cw.fieldref(class, "element", descriptor);
    code.putfield(field, slot_words(ir_ty_to_jvm(&elem)) as i32);
}
