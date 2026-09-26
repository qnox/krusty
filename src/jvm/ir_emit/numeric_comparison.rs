//! JVM emission of a primitive numeric comparison: its operands and its final branch.
//!
//! Checked IR carries a mixed-width comparison's operands already promoted to the compared type,
//! each narrower one through an `ImplicitCoercion`. How that widening is realized is this
//! backend's choice. kotlinc widens a constant operand at compile time, so `d < 1.0F` compares
//! against `dconst_1` and `a == 3` on a `Long` against `ldc2_w 3`, while arithmetic (`d + 1.0F`)
//! keeps its runtime `fconst_1; f2d`. Only the comparison path here pushes a widened constant.

use super::*;

impl Emitter<'_> {
    /// Emit numeric comparison operands and the final branch for both value and branch consumers.
    /// Centralizing the zero-literal rule here is important: operand syntax must not select a different
    /// optimization merely because the surrounding node consumes a Boolean instead of control flow.
    pub(super) fn emit_numeric_compare_branch(
        &mut self,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) {
        use IrBinOp::*;
        // `compare(a, b) <op> 0` IS `a <op> b`. Kotlin's `<`/`<=`/`>`/`>=` are the `compareTo`
        // operator, so the front end models them as a three-way comparison tested against zero —
        // but that result exists only to be tested, and kotlinc emits the direct comparison
        // (`if_icmpge`, or `lcmp` + `ifge`). Unwrapping HERE keeps the operand rules below (the
        // zero-literal form, the int category) as the single place comparisons are shaped.
        if let Some((left, right, direct)) = self.primitive_compare_operands(op, lhs, rhs) {
            self.emit_numeric_compare_branch(direct, left, right, target, jt, code);
            return;
        }
        let lt = self.value_ty(lhs);
        let rt = self.value_ty(rhs);
        if !lt.is_jvm_scalar() || !rt.is_jvm_scalar() {
            crate::trace_compiler!(
                "emit",
                "non-scalar numeric comparison lhs={lhs} {:?} ty={lt:?}, rhs={rhs} {:?} ty={rt:?}",
                self.ir.expr(lhs),
                self.ir.expr(rhs),
            );
        }
        // Numeric. A comparison against the integer literal `0` uses the single-operand compare-to-zero
        // branch (`ifeq`/`iflt`/… — kotlinc's form), saving the `iconst_0`. Only the int category; the
        // others compare 3-way through `lcmp`/`dcmp*`/`fcmp*`, which already tests the result vs 0.
        let int_cat = numeric_cmp_int_category(lt, rt);
        // An unsigned zero is the carrier's zero: equality with it is a compare-to-zero branch too
        // (ordering an unsigned value goes through its comparator, never through here).
        let zero = |e: u32| match self.ir.expr(e) {
            IrExpr::Const(IrConst::Int(0)) => true,
            IrExpr::Const(IrConst::UByte(0) | IrConst::UShort(0) | IrConst::UInt(0)) => {
                matches!(op, Eq | Ne)
            }
            _ => false,
        };
        let cmp0_int = if int_cat && zero(rhs) {
            self.emit_value(lhs, code);
            Some(op)
        } else if int_cat && zero(lhs) && matches!(op, Eq | Ne) {
            // Equality is symmetric, so dropping the left zero preserves kotlinc's bytecode. Ordering
            // deliberately keeps both operands: kotlinc does not rewrite `0 < x` as `x > 0`, and doing
            // so only in branch position was the positional special case this shared path removes.
            self.emit_value(rhs, code);
            Some(op)
        } else {
            self.emit_comparison_operands(lhs, rhs, code);
            None
        };
        // An operand that carried its OWN source line leaves that line in effect. The comparison
        // belongs to the statement around it, so its instruction is marked back to the statement's
        // line — the same "return to the statement's line" the `putfield` of a field store gets.
        if self.statement_line.is_some()
            && [lhs, rhs]
                .iter()
                .any(|operand| self.ir.expr_source_lines.contains_key(operand))
        {
            if let Some(line) = self.statement_line {
                code.mark_line(line);
            }
        }
        if !int_cat {
            // `>`/`>=` use the `*l` float-compare variant, `<`/`<=` the `*g` — so NaN yields false
            // (kotlinc). Long has no NaN distinction but shares the three-way-result branch below.
            let nan_l = matches!(op, Gt | Ge);
            match lt {
                Ty::Long => code.lcmp(),
                Ty::Double => {
                    if nan_l {
                        code.dcmpl()
                    } else {
                        code.dcmpg()
                    }
                }
                Ty::Float => {
                    if nan_l {
                        code.fcmpl()
                    } else {
                        code.fcmpg()
                    }
                }
                _ => unreachable!("int_cat is false only for Long/Double/Float"),
            }
        }
        match cmp0_int {
            Some(o) => cmp0_branch(o, jt, target, code),
            None if !int_cat => cmp0_branch(op, jt, target, code),
            None => icmp_branch(op, jt, target, code),
        }
    }

    /// Push a numeric comparison's operands, each constant one already at the compared width.
    ///
    /// A later operand with control flow is evaluated into temporaries first, as every operand
    /// sequence is; those temporaries hold the operands exactly as checked IR computes them.
    fn emit_comparison_operands(&mut self, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        if self.emits_control_flow(rhs) {
            self.emit_operands(&[lhs, rhs], code);
            return;
        }
        for operand in [lhs, rhs] {
            match self.compared_constant(operand) {
                Some(IrConst::Long(value)) => code.push_long(value, self.cw),
                Some(IrConst::Float(value)) => code.push_float(value, self.cw),
                Some(IrConst::Double(value)) => code.push_double(value, self.cw),
                _ => self.emit_value(operand, code),
            }
        }
    }

    /// `operand` as a constant of its compared width, when it is a numeric constant that checked IR
    /// widens to a wider primitive.
    fn compared_constant(&self, operand: u32) -> Option<IrConst> {
        let IrExpr::TypeOp {
            op: IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } = self.ir.expr(operand)
        else {
            return None;
        };
        let IrExpr::Const(constant) = self.ir.expr(*arg) else {
            return None;
        };
        Some(match (constant, type_operand.canonical_semantic()) {
            (IrConst::Int(value), Ty::Long) => IrConst::Long(i64::from(*value)),
            (IrConst::Int(value), Ty::Float) => IrConst::Float(*value as f32),
            (IrConst::Int(value), Ty::Double) => IrConst::Double(f64::from(*value)),
            (IrConst::Long(value), Ty::Float) => IrConst::Float(*value as f32),
            (IrConst::Long(value), Ty::Double) => IrConst::Double(*value as f64),
            (IrConst::Float(value), Ty::Double) => IrConst::Double(f64::from(*value)),
            _ => return None,
        })
    }
}
