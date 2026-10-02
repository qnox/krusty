//! Source-line attribution at common-IR expression boundaries.
//!
//! Common lowering owns source locations. This module is the JVM debug-info boundary that turns
//! those locations into `LineNumberTable` marks without teaching expression emission how the maps
//! are represented.

use crate::fir::SyntheticOriginKind;
use crate::ir::{ExprId, IrFile, IrNodeOrigin};
use crate::jvm::classfile::CodeBuilder;

use super::Emitter;

/// Mark a source statement or block value at the first instruction it emits.
pub(super) fn mark_statement(ir: &IrFile, expression: ExprId, code: &mut CodeBuilder) {
    if let Some(&line) = ir.expr_lines.get(&expression) {
        code.mark_line(line);
    }
}

/// Withdraw a line marked where a positionless expression begins; see
/// [`CodeBuilder::withdraw_line`].
pub(super) fn begin_expression(ir: &IrFile, expression: ExprId, code: &mut CodeBuilder) {
    if ir.is_positionless(expression) {
        code.withdraw_line();
    }
}

/// Mark the actual return instruction after any active `finally` blocks have run.
///
/// An implicit expression-body return uses the body's closing line. An explicit return uses its own
/// source line, which matters when a finalizer changed the line in effect before control comes back
/// to the pending return. A `return` written as a statement carries that line in the statement map
/// rather than the per-expression one, so both are consulted.
pub(super) fn mark_return(ir: &IrFile, returned: ExprId, code: &mut CodeBuilder) {
    if let Some(line) = ir
        .implicit_return_end_line(returned)
        .or_else(|| ir.expr_source_lines.get(&returned).copied())
        .or_else(|| ir.expr_lines.get(&returned).copied())
    {
        if line != 0 {
            code.mark_line(line);
        }
    }
}

/// Mark a loop's own line at control lowering generated for it: a `for` loop's update and exit
/// test, and the bottom condition of its `do…while` shape.
///
/// kotlinc's `ForLoopsLowering` builds that control at the loop's offsets, and its codegen marks it
/// like any other expression, so after the body the loop's line comes back at the first instruction
/// of the update (`line 7: 74` after the body's `line 8`). `None` — a loop emitted with no statement
/// line in effect — marks nothing, as a node without offsets does in kotlinc.
pub(super) fn mark_loop_control(loop_line: Option<u32>, code: &mut CodeBuilder) {
    if let Some(line) = loop_line {
        if line != 0 {
            code.mark_line(line);
        }
    }
}

/// Mark the line a block's own emission marks first.
///
/// A `finally` is emitted twice: inline on the normal path, and again in the catch-all handler. The
/// handler's entry — the `astore` that parks the in-flight exception — belongs to the finalizer it
/// introduces, so kotlinc gives it the finalizer's FIRST line rather than the `finally` keyword's.
/// Marking it before the store puts both copies of the finalizer on the same line, and the copy's
/// own leading mark then deduplicates.
pub(super) fn mark_block_entry(ir: &IrFile, block: ExprId, code: &mut CodeBuilder) {
    let mut expression = block;
    loop {
        if let Some(&line) = ir.expr_lines.get(&expression) {
            if line != 0 {
                code.mark_line(line);
            }
            return;
        }
        let crate::ir::IrExpr::Block { stmts, value } = ir.expr(expression) else {
            return;
        };
        let Some(&first) = stmts.first().or(value.as_ref()) else {
            return;
        };
        expression = first;
    }
}

/// Mark the `goto` that leaves a `try` after an inlined `finally` copy.
///
/// kotlinc gives that jump the `finally` block's CLOSING line: it belongs to the end of the
/// finalizer, not to whatever statement follows the `try`. Without it the finalizer's own first
/// line stays in effect all the way into the catch-all handler, whose identical mark then
/// deduplicates away — so one missing entry costs two.
pub(super) fn mark_block_exit(ir: &IrFile, block: ExprId, code: &mut CodeBuilder) {
    if let Some(&line) = ir.expr_end_lines.get(&block) {
        if line != 0 {
            code.mark_line(line);
        }
    }
}

impl Emitter<'_> {
    /// Mark one expression line, mapping a same-file inline copy through the class's SMAP. Common
    /// IR supplies the semantic declaration owner and call line; this boundary chooses JVM source
    /// paths and output line numbers.
    pub(super) fn mark_expression_start(&mut self, expression: ExprId, code: &mut CodeBuilder) {
        if self.ir.callable_scopes.contains(&expression) {
            return;
        }
        let Some(&line) = self.ir.expr_source_lines.get(&expression) else {
            return;
        };
        self.mark_expression_line(expression, line, code);
    }

    pub(super) fn mark_expression_line(
        &mut self,
        expression: ExprId,
        line: u32,
        code: &mut CodeBuilder,
    ) {
        if line == 0 {
            return;
        }
        let Some(provenance) = self.ir.inline_copy_provenance(expression) else {
            code.mark_line(line);
            return;
        };
        let Some(call_line) = provenance.call_line else {
            code.mark_line(line);
            return;
        };
        let Some(source_file) = self.cw.source_file_name() else {
            code.mark_line(line);
            return;
        };
        let path = provenance.owner.map_or_else(
            || self.facade.clone(),
            |owner| crate::jvm::names::classfile_internal_name_of(owner).to_owned(),
        );
        let claimable = u16::try_from(self.ir.source_line_count)
            .unwrap_or(u16::MAX)
            .max(1);
        let mapped = self.cw.source_map_for_inlining(claimable).and_then(|map| {
            map.map_copied_line(
                &source_file,
                &path,
                u16::try_from(line).unwrap_or(u16::MAX),
                Some(u16::try_from(call_line).unwrap_or(u16::MAX)),
            )
        });
        match mapped {
            Some(line) => code.mark_line(line.into()),
            None => code.mark_line(line),
        }
    }

    pub(super) fn has_mapped_inline_line(&self, expression: ExprId) -> bool {
        self.ir
            .inline_copy_provenance(expression)
            .is_some_and(|provenance| provenance.call_line.is_some())
            && self.ir.expr_source_lines.contains_key(&expression)
    }

    /// Common IR identifies an operation whose source line owns its generated leading operand.
    /// Retain that line at the operand's start so the positionless operand does not withdraw it and
    /// move it to the later `invoke*`. Ordinary calls keep operand-then-dispatch marking.
    pub(super) fn mark_generated_operand_start(
        &self,
        call: ExprId,
        first_operand: Option<ExprId>,
        code: &mut CodeBuilder,
    ) {
        if !self.ir.starts_at_generated_operand(call)
            || !first_operand.is_some_and(|operand| self.ir.is_positionless(operand))
        {
            return;
        }
        if let Some(line) = self
            .ir
            .dispatch_line(call)
            .or_else(|| self.ir.expr_source_lines.get(&call).copied())
        {
            if line != 0 {
                code.mark_line_retained(line);
            }
        }
    }

    /// Run `emit` as the emission of a `when` branch condition (kotlinc's `isInsideCondition`).
    pub(super) fn in_condition<R>(&mut self, emit: impl FnOnce(&mut Self) -> R) -> R {
        let outer = std::mem::replace(&mut self.inside_condition, true);
        let result = emit(self);
        self.inside_condition = outer;
        result
    }

    /// Emit a `when` branch condition's jump. kotlinc's `visitWhen` keeps a constant source
    /// condition visible to a debugger: it marks the condition's line on a `nop` before deciding
    /// the branch statically. A constant the compiler generated has no source line and no
    /// instruction of its own.
    pub(super) fn emit_when_condition(
        &mut self,
        condition: ExprId,
        target: crate::jvm::classfile::Label,
        jump_when_true: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        let constant = matches!(
            self.ir.expr(condition),
            crate::ir::IrExpr::Const(crate::ir::IrConst::Boolean(_))
        );
        if let Some(&line) = self
            .ir
            .expr_source_lines
            .get(&condition)
            .filter(|_| constant)
        {
            if line != 0 {
                code.mark_line(line);
                code.nop();
            }
        }
        self.in_condition(|this| this.emit_cond_branch(condition, target, jump_when_true, code))
    }

    /// Put a call's own line back in effect at its physical dispatch.
    ///
    /// A multi-line call's operands each mark their own line as they are pushed, so by the time the
    /// `invoke*` is reached the line in effect is the last operand's. kotlinc puts the call's line
    /// back at the dispatch — but only where the instruction really is a dispatch. An operation
    /// lowered to bytecode that calls nothing keeps the operand's line, and marking it would add an
    /// entry kotlinc does not have.
    ///
    /// Which physical operations dispatch is decided per operation, at the site that selects it,
    /// and every answer is pinned by a complete-table differential in
    /// `tests/expression_line_marks_e2e.rs`:
    ///
    /// - every `IrExpr::Call` callee form, `IrExpr::New`, `IrExpr::MethodCall`;
    /// - `IrExpr::InvokeFunction` — a function value's `FunctionN.invoke`;
    /// - `IrExpr::EnumValueOf` — the member `E.valueOf` realization;
    /// - a property read or write whose accessor is a real method, not a field access;
    /// - the intrinsics whose lowering IS a call: `PrimitiveCompare`'s `Integer.compare`,
    ///   `String.get`'s `charAt`.
    ///
    /// Deliberately NOT dispatches: an array read or write, a field read or write, an arithmetic
    /// or comparison instruction, and the reified `enumValueOf<E>` template — that one is an
    /// INLINE expansion, and kotlinc marks its call SITE instead (see
    /// [`Self::mark_inline_call_site_line`]).
    pub(super) fn mark_dispatch_line(&mut self, expression: ExprId, code: &mut CodeBuilder) {
        self.mark_expression_start(expression, code);
        if let Some(line) = self.ir.dispatch_line(expression) {
            if line != 0 {
                self.mark_expression_line(expression, line, code);
            }
        }
    }

    /// kotlinc's `markLineNumberAfterInlineIfNeeded`, after a node that realizes an inlined call in
    /// place (`SyntheticOriginKind::InlinedCall`).
    ///
    /// Inside a condition the line in effect is written again at once, for the jump that follows;
    /// anywhere else it is forgotten, so the next mark of any line is written — kotlinc resets its
    /// last line number after an inlined body, which ran under the callee's lines.
    pub(super) fn mark_after_inlined_call(&self, expression: ExprId, code: &mut CodeBuilder) {
        let Some(IrNodeOrigin::Synthetic {
            kind: SyntheticOriginKind::InlinedCall,
            ..
        }) = self.ir.fir_origins.get(&expression)
        else {
            return;
        };
        match code.current_line() {
            Some(line) if self.inside_condition => code.inlined_line(line),
            _ => code.forget_line(),
        }
    }

    /// Mark the site of an INLINE call, at the first instruction its expansion emits.
    ///
    /// kotlinc keeps this entry even when the expansion's own first instruction begins on the same
    /// offset and a different line, so both source positions survive: `enumValueOf<E>(\n    s\n)`
    /// records the call's line and its argument's line at one pc. An inline expansion has no
    /// dispatch of its own to return to afterwards — the `invoke*` it ends with belongs to the
    /// inlined body, not to the call — so this REPLACES the dispatch mark rather than joining it.
    pub(super) fn mark_inline_call_site_line(&self, expression: ExprId, code: &mut CodeBuilder) {
        if let Some(&line) = self.ir.expr_source_lines.get(&expression) {
            if line != 0 {
                code.mark_line_retained(line);
            }
        }
    }
}
