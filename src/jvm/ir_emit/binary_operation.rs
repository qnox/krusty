//! JVM emission for checked binary operations and their operand/debug-line ordering.

use super::{emit_num_conv, ir_ty_to_jvm, CodeBuilder, Emitter, TempRole, Ty};
use crate::ir::IrBinOp;

impl Emitter<'_> {
    fn is_unsigned_bitwise_result(&self, expression: u32) -> bool {
        self.ir
            .logical_types
            .get(&expression)
            .is_some_and(|ty| matches!(ty.canonical_semantic().non_null(), Ty::UInt | Ty::ULong))
    }

    pub(super) fn mark_value_expression_start(&mut self, expression: u32, code: &mut CodeBuilder) {
        // A plain local operand of an inline-only unsigned member is loaded with no line of its
        // own (`genOrGetLocal`).
        if self.suppress_unsigned_local_line {
            return;
        }
        if crate::jvm::line_numbers::marks_line_at_start(self.ir, expression)
            || self.keeps_start_line(expression)
        {
            self.mark_expression_start(expression, code);
        }
    }

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
                let unsigned = self.is_unsigned_bitwise_result(expression);
                if unsigned && !self.spills_operand_prefix(rhs) {
                    self.emit_unsigned_bitwise_operands(expression, lhs, rhs, code);
                } else if let Some(line) =
                    super::debug_lines::inversion_mask_line(self.ir, expression)
                {
                    // kotlinc's `inv` intrinsic restates its line where it pushes the mask.
                    self.emit_value(lhs, code);
                    self.mark_expression_line(expression, line, code);
                    self.emit_value(rhs, code);
                    self.mark_expression_start(expression, code);
                } else {
                    self.emit_binary_operands_with_live_prefix(lhs, rhs, code);
                    self.mark_expression_start(expression, code);
                }
                // A checked unsigned operand can retain its semantic identity after value-class
                // rewriting even though emission has put its signed carrier on the stack. Opcode
                // width is a JVM representation decision, so select it from that carrier rather
                // than treating every non-`Ty::Long` identity as an int-category value.
                match ir_ty_to_jvm(&lt) {
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
                self.rebuild_unsigned_bitwise_result(expression, code);
            }
            Shl | Shr | Ushr => {
                let unsigned = self.is_unsigned_bitwise_result(expression);
                if unsigned && !self.spills_operand_prefix(rhs) {
                    self.emit_unsigned_bitwise_operands(expression, lhs, rhs, code);
                } else {
                    self.emit_operands(&[lhs, rhs], code);
                    self.mark_expression_start(expression, code);
                }
                match ir_ty_to_jvm(&lt) {
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
                self.rebuild_unsigned_bitwise_result(expression, code);
            }
            Lt | Le | Gt | Ge | Eq | Ne | RefEq | RefNe => self.emit_comparison(expression, code),
        }
    }

    /// `UInt` and `ULong` bitwise and shift results are the carrier opcode plus `constructor-impl`.
    /// Checked FIR publishes that result as the operation's logical type. A signed operation, and an
    /// inlined arithmetic body whose carrier opcode is already the argument of `constructor-impl`,
    /// keeps the opcode alone.
    fn rebuild_unsigned_bitwise_result(&mut self, expression: u32, code: &mut CodeBuilder) {
        let semantic = self
            .ir
            .logical_types
            .get(&expression)
            .copied()
            .map(|ty| ty.canonical_semantic().non_null());
        let Some(semantic @ (Ty::UInt | Ty::ULong)) = semantic else {
            return;
        };
        if super::value_class_adapters::emit_native_value_class_constructor(self.cw, code, semantic)
        {
            // These members are inline-only. kotlinc's `markLineNumberAfterInlineIfNeeded` runs
            // after the body: inside a condition the line is written again at once, on the jump;
            // otherwise it is forgotten so the next mark of that same line is kept.
            code.line_after_inline(self.inside_condition);
        }
    }

    /// The operands of an unsigned bitwise member, which the standard library declares
    /// `@InlineOnly` over the signed carrier, and the member's own line.
    ///
    /// kotlinc inlines the member with its arguments in place (`IrInlineCodegen`): a plain local
    /// argument is `genOrGetLocal`, loaded with no line, any other argument is generated with its
    /// own lines, and the call marks its line once they are, ahead of their code (see
    /// `CodeBuilder::mark_in_place_call_line`). The body itself has no lines, and the mapper
    /// forgets the caller's line after it (`rebuild_unsigned_bitwise_result`).
    fn emit_unsigned_bitwise_operands(
        &mut self,
        expression: u32,
        lhs: u32,
        rhs: u32,
        code: &mut CodeBuilder,
    ) {
        let index = code.line_entry_count();
        let pc = code.bytes.len().min(usize::from(u16::MAX)) as u16;
        self.emit_unsigned_operand(lhs, code);
        self.emit_unsigned_operand(rhs, code);
        if let Some(line) = self.expression_start_line(expression, code) {
            code.mark_in_place_call_line(index, pc, line);
        }
    }

    /// One argument of an unsigned member. A plain local is `genOrGetLocal`, loaded with no line.
    /// An operand lowering generated with no position (`inv()`'s all-bits mask) is code of the
    /// member's own body, which has no lines either.
    fn emit_unsigned_operand(&mut self, operand: u32, code: &mut CodeBuilder) {
        let plain_local = self.plain_local_operand(operand);
        let body_code = self.ir.is_positionless(operand);
        let saved = self.suppress_unsigned_local_line;
        if plain_local {
            self.suppress_unsigned_local_line = true;
        }
        if body_code {
            code.line_mapper().open_no_line_scope();
        }
        self.emit_value(operand, code);
        if body_code {
            code.line_mapper().close_no_line_scope();
        }
        self.suppress_unsigned_local_line = saved;
    }

    fn plain_local_operand(&self, mut expression: u32) -> bool {
        loop {
            match self.ir.expr(expression) {
                crate::ir::IrExpr::TypeOp { arg, .. } => expression = *arg,
                crate::ir::IrExpr::Block {
                    stmts,
                    value: Some(value),
                } if stmts.is_empty() => expression = *value,
                crate::ir::IrExpr::GetValue(_) => return true,
                _ => return false,
            }
        }
    }
}
