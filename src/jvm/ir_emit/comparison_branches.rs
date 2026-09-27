//! JVM emission of a comparison (`<`, `<=`, `>`, `>=`, `==`, `!=`, `===`, `!==`) in value position
//! and as a condition: the null, identity, structural and numeric forms share one classifier, and
//! the jump carries the comparison's own source line, as kotlinc's `BooleanComparison` marks it.

use super::*;
use crate::ir::ExprId;

impl Emitter<'_> {
    /// A comparison in value position: its Boolean result on the operand stack.
    pub(super) fn emit_comparison(&mut self, expression: ExprId, code: &mut CodeBuilder) {
        let (op, lhs, rhs) = self.comparison_parts(expression);
        self.at_comparison_line(expression, |this| {
            this.emit_comparison_value(op, lhs, rhs, code)
        });
    }

    /// A comparison as a condition: one fused branch to `target`, taken when its result is `jt`.
    pub(super) fn emit_comparison_branch(
        &mut self,
        expression: ExprId,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) {
        let (op, lhs, rhs) = self.comparison_parts(expression);
        self.at_comparison_line(expression, |this| {
            this.emit_compare_branch(op, lhs, rhs, target, jt, code)
        });
    }

    fn comparison_parts(&self, expression: ExprId) -> (IrBinOp, ExprId, ExprId) {
        match *self.ir.expr(expression) {
            IrExpr::PrimitiveBinOp { op, lhs, rhs } => (op, lhs, rhs),
            ref other => panic!("a comparison is a primitive binary operation, not {other:?}"),
        }
    }

    /// Emit `expression`'s comparison with its own source line as the one its jump carries.
    ///
    /// kotlinc's `BooleanComparison` marks the comparison call's line (`markLineNumber(expression)`)
    /// right before the jump instruction, after its operands have marked theirs. A comparison
    /// without a line of its own leaves the enclosing statement's in effect instead (see
    /// `emit_numeric_compare_branch`).
    fn at_comparison_line<R>(
        &mut self,
        expression: ExprId,
        emit: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let line = self.ir.expr_source_lines.get(&expression).copied();
        let outer = std::mem::replace(&mut self.comparison_line, line);
        let result = emit(self);
        self.comparison_line = outer;
        result
    }

    fn emit_comparison_value(&mut self, op: IrBinOp, lhs: u32, rhs: u32, code: &mut CodeBuilder) {
        let f = code.new_label();
        // Every comparison that needs a conditional branch goes through the same classifier and
        // operand emitter used by `if`/`while`/`when`. Value position merely supplies a false target
        // and materializes the resulting 0/1. This is intentionally one semantic path: keeping separate
        // null/reference/numeric case tables here previously let zero-left ordering acquire a different
        // node-shape rule depending on whether the comparison happened to be an `if` condition.
        if self.emit_non_structural_compare_branch(op, lhs, rhs, f, false, code) {
            self.materialize_cmp_bool(f, code);
            return;
        }

        // The shared emitter returns false only for structural equality between two non-null
        // references. `Intrinsics.areEqual` already produces the Boolean value kotlinc returns in value
        // position, so branching merely to reconstruct it would be longer and less faithful.
        self.emit_structural_equality(lhs, rhs, code);
        if op == IrBinOp::Ne {
            code.push_int(1, self.cw);
            code.ixor();
        }
    }

    /// Tail of a value-position comparison: the caller has emitted a conditional branch to `f` taken
    /// exactly when the comparison is FALSE. Fall through to `iconst_1`, jump over the `iconst_0` the
    /// `f` arm pushes — kotlinc's polarity (`if_icmpne; iconst_1; goto; iconst_0`), which keeps the
    /// null, referential and numeric arms byte-identical to it at no extra instruction cost.
    pub(super) fn materialize_cmp_bool(&mut self, f: Label, code: &mut CodeBuilder) {
        // The branch popped its operands — this is the height on BOTH merge paths (the `f` branch and
        // the fall-through). The 0/1 booleans below each leave exactly one value, so the tracker must be
        // reset to this height at `bind(f)`; otherwise the linear counter carries the fall-through's
        // `push 1` past the `goto`, drifting `cur_stack` +1 (harmless for max_stack, but it makes
        // `stack_height()` over-report, which the branchy-inline baseline check relies on).
        let merged = code.stack_height().max(0) as u16;
        let end = code.new_label();
        code.push_int(1, self.cw);
        code.goto(end);
        self.bind(f, code);
        code.set_stack(merged);
        code.push_int(0, self.cw);
        self.bind(end, code);
    }

    /// Emit the comparison `lhs <op> rhs` directly as a single conditional jump to `target`, taken when
    /// the comparison's result equals `jt` — no 0/1 boolean is materialized. Mirrors `emit_comparison_value`'s
    /// operand/3-way/null/ref handling but ends in one fused branch with the right polarity.
    fn emit_compare_branch(
        &mut self,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) {
        if self.emit_non_structural_compare_branch(op, lhs, rhs, target, jt, code) {
            return;
        }

        // The shared classifier leaves only non-null structural `==`/`!=` here. Unlike value position,
        // a condition must consume `Intrinsics.areEqual` with one final branch; the comparison's
        // requested polarity determines whether equality means taking or skipping the target.
        debug_assert!(matches!(op, IrBinOp::Eq | IrBinOp::Ne));
        self.emit_structural_equality(lhs, rhs, code);
        if (op == IrBinOp::Eq) == jt {
            code.ifne(target);
        } else {
            code.ifeq(target);
        }
    }

    /// Emit every comparison except non-null structural reference equality as a branch.
    ///
    /// Returning `false` is a deliberately narrow contract: both operands are non-null references and
    /// `op` is `==`/`!=`, so the caller must emit `Intrinsics.areEqual` in the form appropriate to its
    /// consumer. All null, identity and numeric classification lives here so comparison semantics cannot
    /// drift based on whether an identical IR node is consumed as a Boolean value or as control flow.
    fn emit_non_structural_compare_branch(
        &mut self,
        op: IrBinOp,
        lhs: u32,
        rhs: u32,
        target: Label,
        jt: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        use IrBinOp::*;
        let lt = self.value_ty(lhs);
        // `x == null` / `x != null` / `x === null` / `x !== null` → single-operand `ifnull`/`ifnonnull`
        // (kotlinc's form), NOT `aconst_null; if_acmp*`. Computed up front so the referential-identity
        // path below doesn't claim a null comparison (a `null` literal's type is a reference).
        let lhs_null = matches!(self.ir.expr(lhs), IrExpr::Const(IrConst::Null));
        let rhs_null = matches!(self.ir.expr(rhs), IrExpr::Const(IrConst::Null));
        // Referential identity (`===`/`!==`) on two non-null references — or on a mixed reference/
        // primitive pair, whose primitive side boxes first — → `if_acmpeq`/`if_acmpne`.
        if matches!(op, RefEq | RefNe)
            && identity_compares_refs(lt, self.value_ty(rhs))
            && !lhs_null
            && !rhs_null
        {
            self.emit_identity_operands(lhs, rhs, code);
            if (op == RefEq) == jt {
                code.if_acmpeq(target);
            } else {
                code.if_acmpne(target);
            }
            return true;
        }
        let op = match op {
            RefEq => Eq,
            RefNe => Ne,
            o => o,
        };
        if matches!(op, Eq | Ne) && (lhs_null || rhs_null) {
            let operand = if lhs_null { rhs } else { lhs };
            // A physical primitive arises here only for identity (`x === null`/`x !== null`): kotlinc
            // accepts that with an always-false/true warning, whereas structural `x == null` is
            // rejected by the front end. Use the same adapted-operand primitive as mixed identity so
            // the `ifnull` reference slot receives a box; reference structural operands are a no-op.
            self.emit_operands_adapted(None, &[operand], code, Self::box_scalar_operand);
            if (op == Eq) == jt {
                code.ifnull(target);
            } else {
                code.ifnonnull(target);
            }
            return true;
        }
        // Structural equality's value result has different optimal consumers: value position can use it
        // directly, while control flow branches on it. Tell the caller to select that final operation;
        // the semantic classification itself still occurs once, here.
        if matches!(op, Eq | Ne) && !(lt.is_jvm_scalar() && self.value_ty(rhs).is_jvm_scalar()) {
            return false;
        }
        self.emit_numeric_compare_branch(op, lhs, rhs, target, jt, code);
        true
    }
}
