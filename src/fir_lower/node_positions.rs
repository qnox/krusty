//! The source positions lowering gives the IR nodes it builds.
//!
//! fir2ir passes the offsets of the FIR element it converts to every IR element it builds for it.
//! Lowering does the same in line form: once a FIR expression or statement is lowered, every node
//! built for it that nothing settled yet takes its position (see [`crate::ir::IrNodePosition`]). A
//! node an inner expression built keeps that expression's position, a copy keeps its source's, and
//! a node a body builds outside any of its expressions has none. Which of these positions open a
//! line is a backend's decision; lowering only records them.

use crate::fir::{FirExprId, FirExprKind, FirExpressionDebugLines, FirStatementId};
use crate::ir::{ExprId, IrNodePosition};

use super::BodyLowering;

fn line(line: u32) -> Option<u32> {
    (line != 0).then_some(line)
}

impl BodyLowering<'_> {
    /// The lines of `expression`, as its lowered nodes carry them.
    pub(super) fn expression_position(
        &self,
        expression: FirExprId,
        kind: &FirExprKind,
    ) -> FirExpressionDebugLines {
        let mut debug = self.body.expression_debug_lines(expression);
        // kotlinc replaces an array-factory call (`intArrayOf(a, b)`) by its vararg, whose offsets
        // start at the first element, so the call's own line never opens. An empty factory call
        // keeps its own position.
        if let FirExprKind::ArrayLiteral { elements, .. } = kind {
            let first = elements
                .first()
                .map(|first| self.body.expression_debug_lines(first.value));
            if let Some(first) = first.filter(|first| first.source != 0) {
                debug.start = first.start;
                debug.source = first.source;
            }
        }
        debug
    }

    /// Give the nodes built from `first_generated` on for one FIR expression its position.
    /// `lowered` is the node the expression lowered to.
    pub(super) fn stamp_expression(
        &mut self,
        debug: FirExpressionDebugLines,
        lowered: ExprId,
        first_generated: usize,
    ) {
        if debug.positionless {
            self.ir.mark_positionless(lowered);
        }
        if debug.source != 0 {
            self.ir.set_source_line(lowered, debug.source);
            // A call whose operands were stored first keeps the call's line on the call itself, which
            // kotlinc marks once those operands are evaluated.
            if let Some(&call) = self.operand_bound_calls.get(&lowered) {
                self.ir.set_source_line_if_absent(call, debug.source);
            }
            for raw in first_generated..self.ir.exprs.len() {
                let raw = raw as u32;
                if !self.ir.suspend_calls.contains_key(&raw) {
                    continue;
                }
                self.ir.set_source_line_if_absent(raw, debug.source);
            }
        }
        if debug.end != 0 {
            self.ir.set_end_line(lowered, debug.end);
            for raw in first_generated..self.ir.exprs.len() {
                let raw = raw as u32;
                if !self.ir.suspend_calls.contains_key(&raw) {
                    continue;
                }
                self.ir.set_end_line_if_absent(raw, debug.end);
            }
        }
        // kotlinc builds a positionless expression without offsets, so the nodes built for it have
        // none either.
        let position = if debug.positionless {
            IrNodePosition::default()
        } else {
            IrNodePosition {
                start_line: line(debug.start),
                source_line: line(debug.source),
                end_line: line(debug.end),
                ..IrNodePosition::default()
            }
        };
        if let Some(start) = position.start_line {
            self.ir.set_start_line_if_absent(lowered, start);
        }
        self.ir
            .settle_positions(first_generated..self.ir.exprs.len(), position);
    }

    /// Give the nodes built from `first_generated` on for one FIR statement its line.
    pub(super) fn stamp_statement(&mut self, statement: FirStatementId, first_generated: usize) {
        let position = IrNodePosition {
            source_line: line(self.body.statement_debug_line(statement)),
            ..IrNodePosition::default()
        };
        self.ir
            .settle_positions(first_generated..self.ir.exprs.len(), position);
    }

    /// Settle the nodes an external inline function's body expanded to from `first_expanded` on.
    /// They are that function's code, which kotlinc copies with its own lines, not code built for
    /// the call: a node the expansion gave no line keeps none, and the operands lowered for the
    /// call keep their own positions.
    pub(super) fn settle_external_expansion_positions(&mut self, first_expanded: usize) {
        self.ir.settle_positions(
            first_expanded..self.ir.exprs.len(),
            IrNodePosition::default(),
        );
    }

    /// Settle the nodes this body built outside any of its expressions and statements, which have
    /// no source position, so an enclosing expression does not stamp them as its own.
    pub(super) fn settle_body_positions(&mut self) {
        self.ir.settle_positions(
            self.first_expression..self.ir.exprs.len(),
            IrNodePosition::default(),
        );
    }
}
