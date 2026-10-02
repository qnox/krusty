//! JVM emission for checked binary operations and their operand/debug-line ordering.

use super::{emit_num_conv, ir_ty_to_jvm, CodeBuilder, Emitter, TempRole, Ty};
use crate::ir::IrBinOp;

impl Emitter<'_> {
    pub(super) fn emit_binop(
        &mut self,
        expression: u32,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
        code: &mut CodeBuilder,
    ) {
        use IrBinOp::*;
        let lt = self.value_ty(lhs);
        match op {
            Add | Sub | Mul | Div | Rem => {
                // A branchy RHS (`result*31 + <nullable-field hashCode ternary>`): keep the numeric LHS on
                // the operand stack across the RHS's branch — matching kotlinc. Final-bytecode analysis
                // carries that prefix through every edge, so emitter-side frame annotation is unnecessary. A
                // non-branchy RHS (or a branchy LHS) keeps the ordinary `emit_operands` path (spill only if
                // needed) — bytecode unchanged for the common case. NOT applicable when the RHS can enter
                // an exception handler, suspend, or transfer to an enclosing loop (`must_spill_across`):
                // those boundaries cannot carry the held LHS to this operation.
                if self.emits_control_flow(rhs)
                    && !self.emits_control_flow(lhs)
                    && !self.must_spill_across(rhs)
                {
                    self.emit_value(lhs, code);
                    self.emit_value(rhs, code);
                } else {
                    self.emit_operands(&[lhs, rhs], code);
                }
                // The operands may have moved the line to a continuation. kotlinc puts the
                // operation back on the expression's own start line.
                self.mark_expression_start(expression, code);
                match lt {
                    Ty::Long => match op {
                        Add => code.ladd(),
                        Sub => code.lsub(),
                        Mul => code.lmul(),
                        Div => code.ldiv(),
                        Rem => code.lrem(),
                        _ => unreachable!(),
                    },
                    Ty::Double => match op {
                        Add => code.dadd(),
                        Sub => code.dsub(),
                        Mul => code.dmul(),
                        Div => code.ddiv(),
                        Rem => code.drem(),
                        _ => unreachable!(),
                    },
                    Ty::Float => match op {
                        Add => code.fadd(),
                        Sub => code.fsub(),
                        Mul => code.fmul(),
                        Div => code.fdiv(),
                        Rem => code.frem(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        Add => code.iadd(),
                        Sub => code.isub(),
                        Mul => code.imul(),
                        Div => code.idiv(),
                        Rem => code.irem(),
                        _ => unreachable!(),
                    },
                }
                // JVM int-category arithmetic always leaves a full `int`, even when both inputs
                // use the Char/Byte/Short carrier. Checked FIR's result type remains authoritative:
                // normalize only after the operation (`Char + Int` wraps modulo 2^16), while an
                // ordinary `Char - Char : Int` remains untouched.
                let arithmetic_result = match lt {
                    Ty::Byte | Ty::Short | Ty::Char => Ty::Int,
                    other => other,
                };
                let semantic_result = self
                    .ir
                    .logical_types
                    .get(&expression)
                    .copied()
                    .unwrap_or(arithmetic_result);
                emit_num_conv(
                    arithmetic_result,
                    ir_ty_to_jvm(&semantic_result.non_null()),
                    code,
                );
            }
            And | Or => {
                // Evaluate lhs, hold it in a temp while a branchy rhs is emitted, then combine. The
                // temp is dead afterwards, so release it so it doesn't leak into later merge frames.
                // Without this, a `false`/`else` path that never assigned the temp reaches a merge
                // whose frame claims it's defined → VerifyError.
                self.emit_value(lhs, code);
                let temp = self.frame.enter_temp(TempRole::BooleanOperand, Ty::Boolean);
                let tmp = temp.slot();
                let lease = self.lease_frame_temporary(temp, Ty::Boolean);
                code.istore(tmp);
                self.emit_value(rhs, code);
                code.iload(tmp);
                if op == And {
                    code.iand()
                } else {
                    code.ior()
                }
                self.release_temporary(lease);
            }
            BitAnd | BitOr | BitXor => {
                self.emit_binary_operands_with_live_prefix(lhs, rhs, code);
                self.mark_expression_start(expression, code);
                match lt {
                    Ty::Long => match op {
                        BitAnd => code.land(),
                        BitOr => code.lor(),
                        BitXor => code.lxor(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        BitAnd => code.iand(),
                        BitOr => code.ior(),
                        BitXor => code.ixor(),
                        _ => unreachable!(),
                    },
                }
            }
            Shl | Shr | Ushr => {
                self.emit_operands(&[lhs, rhs], code); // shift amount is an `Int`
                self.mark_expression_start(expression, code);
                match lt {
                    Ty::Long => match op {
                        Shl => code.lshl(),
                        Shr => code.lshr(),
                        Ushr => code.lushr(),
                        _ => unreachable!(),
                    },
                    _ => match op {
                        Shl => code.ishl(),
                        Shr => code.ishr(),
                        Ushr => code.iushr(),
                        _ => unreachable!(),
                    },
                }
            }
            Lt | Le | Gt | Ge | Eq | Ne | RefEq | RefNe => self.emit_comparison(expression, code),
        }
    }
}
