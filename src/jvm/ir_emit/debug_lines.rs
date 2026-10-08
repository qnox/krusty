//! Source-line attribution at common-IR expression boundaries.
//!
//! Common lowering owns source locations. This module is the JVM debug-info boundary that turns
//! those locations into `LineNumberTable` marks without teaching expression emission how the maps
//! are represented.

use crate::fir::SyntheticOriginKind;
use crate::ir::{ExprId, IrExpr, IrFile, IrNodeOrigin};
use crate::jvm::classfile::CodeBuilder;

use super::Emitter;

fn first_block_expression(ir: &IrFile, block: ExprId) -> ExprId {
    let mut expression = block;
    loop {
        let crate::ir::IrExpr::Block { stmts, value } = ir.expr(expression) else {
            return expression;
        };
        let Some(&first) = stmts.first().or(value.as_ref()) else {
            return expression;
        };
        expression = first;
    }
}

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

impl Emitter<'_> {
    /// Mark the line a catch-all finalizer copy's block emission marks first.
    ///
    /// A `finally` is emitted inline on the normal path and again at a handler entry. The exception
    /// edge resets kotlinc's last-line state, so the handler keeps an entry even when the normal
    /// copy ended on that same line. Mapping through the first expression's inline provenance also
    /// gives a copied finalizer the caller line that its body uses, rather than the callee's raw line.
    pub(super) fn mark_finalizer_handler_entry(
        &mut self,
        try_expression: ExprId,
        block: ExprId,
        code: &mut CodeBuilder,
    ) {
        let mut expression = first_block_expression(self.ir, block);
        let entry = loop {
            if let Some(&line) = self.ir.expr_lines.get(&expression) {
                break (line != 0).then_some((expression, line));
            }
            let crate::ir::IrExpr::Block { stmts, value } = self.ir.expr(expression) else {
                break None;
            };
            let Some(&first) = stmts.first().or(value.as_ref()) else {
                break None;
            };
            expression = first;
        };
        if let Some((expression, line)) = entry {
            self.note_inlined_expression(expression, code);
            let line = self.mapped_expression_line(expression, line);
            // A handler reopens the finalizer's line after a preceding, different `try` line even
            // when the normal-path finalizer left that same line in effect. An entirely one-line
            // `try { ... } finally { ... }` has no line transition at all, and kotlinc keeps only
            // the entry at pc 0 instead of repeating it at the catch-all handler.
            if self.ir.expr_source_lines.get(&try_expression).copied()
                == self.ir.expr_lines.get(&expression).copied()
            {
                code.mark_line(line);
            } else {
                code.mark_control_entry_line(line);
            }
        }
    }

    /// Mark the actual return instruction after any active `finally` blocks have run.
    ///
    /// An implicit expression-body return uses the body's closing line. An explicit return uses
    /// its own source line, which matters when a finalizer changed the line in effect before
    /// control comes back to the pending return. A copied inline return keeps that semantic line,
    /// while this JVM boundary maps it through the enclosing class's source map like every other
    /// copied expression.
    pub(super) fn mark_return(&mut self, returned: ExprId, code: &mut CodeBuilder) {
        let implicit_end = self.ir.implicit_return_end_line(returned).filter(|_| {
            !matches!(self.ir.expr(returned), IrExpr::Return(Some(value))
                if super::safe_calls::is_safe_call_result(self.ir, *value))
        });
        if let Some(line) = implicit_end
            .or_else(|| self.ir.expr_source_lines.get(&returned).copied())
            .or_else(|| self.ir.expr_lines.get(&returned).copied())
        {
            self.mark_expression_line(returned, line, code);
        }
    }

    /// Mark one expression line, mapping a same-module inline copy through the class's SMAP. Common
    /// IR supplies the semantic declaration owner, the callee's source file, and the call line;
    /// this boundary chooses JVM source paths and output line numbers.
    pub(super) fn mark_expression_start(&mut self, expression: ExprId, code: &mut CodeBuilder) {
        if self.ir.callable_scopes.contains(&expression)
            || self.ir.dispatch_line(expression).is_some()
        {
            return;
        }
        let Some(&line) = self.ir.expr_source_lines.get(&expression) else {
            return;
        };
        self.mark_expression_line(expression, line, code);
    }

    /// Mark `line`, mapping an inline copy through the class SMAP. Returns the line written into
    /// the `LineNumberTable` (the mapped output line when a map answers), or `0` when `line` is
    /// `0` and nothing is marked.
    pub(super) fn mark_expression_line(
        &mut self,
        expression: ExprId,
        line: u32,
        code: &mut CodeBuilder,
    ) -> u32 {
        if line == 0 {
            return 0;
        }
        self.note_inlined_expression(expression, code);
        let line = self.mapped_expression_line(expression, line);
        code.mark_line(line);
        line
    }

    /// An inline copy sorts its exception table the way kotlinc's inliner does. The flag follows
    /// the expression, so a mapped line and a finalizer handler that reopens the same line both
    /// record it.
    fn note_inlined_expression(&self, expression: ExprId, code: &mut CodeBuilder) {
        let inlined = self.ir.external_frame_lines.contains_key(&expression)
            || self.ir.inline_synthetic_lines.contains(&expression)
            || self
                .ir
                .inline_copy_provenance(expression)
                .is_some_and(|provenance| provenance.iter().any(|frame| frame.call_line.is_some()));
        if inlined {
            code.note_inlined_bytecode();
        }
    }

    fn mapped_expression_line(&mut self, expression: ExprId, line: u32) -> u32 {
        // A line of an external inline declaration's own body maps through the class's SMAP under
        // the call that expanded it (kotlinc's `mapLineNumber` for the inline interval).
        if let Some(frame) = self.ir.external_frame_lines.get(&expression).cloned() {
            return self.map_external_frame_line(&frame);
        }
        // An `@InlineOnly` expansion's lambda marker separates the call's line from a lambda body
        // starting on that same line with the target's synthetic inline line (kotlinc's
        // `mapSyntheticLineNumber(1)`, recorded as `fake.kt`).
        if self.ir.inline_synthetic_lines.contains(&expression) {
            let claimable = u16::try_from(self.ir.source_line_count)
                .unwrap_or(u16::MAX)
                .max(1);
            if let Some(synthetic) = self
                .cw
                .source_map_for_inlining(claimable)
                .and_then(|map| map.map_synthetic_line(1))
            {
                return u32::from(synthetic);
            }
        }
        // The source line stays the declaration's raw line. A nested same-module expansion has
        // several semantic frames, but this physical copy is emitted from the outermost body;
        // kotlinc maps that raw line under the outer call instead of recursively feeding an inner
        // synthetic output line back into the same source map.
        let Some(frame) = self
            .ir
            .inline_copy_provenance(expression)
            .and_then(|provenance| {
                provenance
                    .iter()
                    .rev()
                    .find(|frame| frame.call_line.is_some())
            })
            .copied()
        else {
            return line;
        };
        let call_line = frame
            .call_line
            .expect("an inline frame with a call line was selected");
        let Some((source_file, path)) =
            self.copied_inline_file(frame.function, frame.owner, frame.source)
        else {
            return line;
        };
        let claimable = u16::try_from(self.ir.source_line_count)
            .unwrap_or(u16::MAX)
            .max(1);
        self.cw
            .source_map_for_inlining(claimable)
            .and_then(|map| {
                map.map_copied_line(
                    &source_file,
                    &path,
                    u16::try_from(line).unwrap_or(u16::MAX),
                    Some(u16::try_from(call_line).unwrap_or(u16::MAX)),
                )
            })
            .map_or(line, u32::from)
    }

    /// The source file and JVM path a copied inline line is mapped under.
    ///
    /// A top-level callee uses the exact facade already resolved for its declaration. A member uses
    /// its semantic classifier. Same-file top-level code uses the current facade.
    fn copied_inline_file(
        &self,
        function: crate::ir::FunId,
        owner: Option<crate::types::TypeName>,
        source: crate::fir::SourceFileId,
    ) -> Option<(String, String)> {
        let identity = self.ir.source_debug.file(source)?;
        let path = owner.map_or_else(
            || {
                if self.ir.source_debug.is_current(source) {
                    Some(self.facade.clone())
                } else {
                    self.ir.foreign_template_facade(function).map(|facade| {
                        crate::jvm::names::classfile_internal_name_of(facade).to_owned()
                    })
                }
            },
            |owner| Some(crate::jvm::names::classfile_internal_name_of(owner).to_owned()),
        )?;
        Some((identity.name.to_string(), path))
    }

    /// Mark the `goto` that leaves a `try` after an inlined `finally` copy, through the class
    /// SMAP when the finalizer is an inline copy.
    ///
    /// kotlinc gives that jump the `finally` block's closing line. Without it the finalizer's own
    /// first line stays in effect into the catch-all handler, whose identical mark can then
    /// deduplicate away.
    pub(super) fn mark_block_exit(&mut self, block: ExprId, code: &mut CodeBuilder) {
        if let Some(&line) = self.ir.expr_end_lines.get(&block) {
            if line != 0 {
                self.mark_expression_line(block, line, code);
            }
        }
    }

    /// Map a line of an external inline declaration's body through the class's source map, under
    /// the call that expanded it; the raw line when no map answers.
    pub(super) fn map_external_frame_line(
        &mut self,
        frame: &crate::ir::IrExternalFrameLine,
    ) -> u32 {
        let claimable = u16::try_from(self.ir.source_line_count)
            .unwrap_or(u16::MAX)
            .max(1);
        self.cw
            .source_map_for_inlining(claimable)
            .and_then(|map| {
                map.map_line(
                    &frame.file,
                    &frame.path,
                    u16::try_from(frame.line).unwrap_or(u16::MAX),
                    u16::try_from(frame.call_line).unwrap_or(u16::MAX),
                )
            })
            .map_or(frame.line, u32::from)
    }

    pub(super) fn has_retained_mapped_inline_unit_line(&self, expression: ExprId) -> bool {
        self.ir.retains_inline_unit_line(expression)
            && self
                .ir
                .inline_copy_provenance(expression)
                .is_some_and(|provenance| provenance.iter().any(|frame| frame.call_line.is_some()))
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

    /// Emit a `when` branch condition's jump (kotlinc's `isInsideCondition` around the jump).
    pub(super) fn emit_when_condition(
        &mut self,
        condition: ExprId,
        target: crate::jvm::classfile::Label,
        jump_when_true: bool,
        code: &mut CodeBuilder,
    ) -> bool {
        self.in_condition(|this| this.emit_cond_branch(condition, target, jump_when_true, code))
    }

    /// kotlinc's `visitConst` for a Boolean: a source constant marks its line on a `nop`, because a
    /// Boolean constant need not be materialized and the debugger still has to stop on its line.
    /// A constant condition is decided statically, so `while (true)`, `do … while (false)` and a
    /// constant `when` branch condition keep their line only through that `nop`. A constant the
    /// compiler generated has no source line and no instruction of its own; the `nop` of a line
    /// that has other instructions is cleaned up with every other redundant `nop`.
    pub(super) fn mark_boolean_constant(&mut self, constant: ExprId, code: &mut CodeBuilder) {
        let Some(&line) = self.ir.expr_source_lines.get(&constant) else {
            return;
        };
        if line != 0 {
            self.mark_expression_line(constant, line, code);
            code.nop();
        }
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
    /// - the intrinsics whose lowering IS a call: `PrimitiveCompare`'s `Intrinsics.compare`,
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
    /// place (`SyntheticOriginKind::InlinedCall`), and after an external-inline expansion lowered
    /// from its declaration plan.
    ///
    /// Inside a condition the line in effect is written again at once, for the jump that follows;
    /// anywhere else it is forgotten, so the next mark of any line is written — kotlinc resets its
    /// last line number after an inlined body, which ran under the callee's lines.
    pub(super) fn mark_after_inlined_call(&self, expression: ExprId, code: &mut CodeBuilder) {
        let inlined = self.ir.external_inline_expansions.contains(&expression)
            || matches!(
                self.ir.fir_origins.get(&expression),
                Some(IrNodeOrigin::Synthetic {
                    kind: SyntheticOriginKind::InlinedCall,
                    ..
                })
            );
        if !inlined {
            return;
        }
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
