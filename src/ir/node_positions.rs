//! The source position every expression node carries, kotlinc's `IrElement` offsets in line form.
//!
//! A position belongs to the node, not to a table a pass must remember: it lives in a column
//! parallel to [`IrFile::exprs`], so a copy of the node takes it with the node's other facts
//! ([`InheritedFacts`]). A node built without a source location has none.
//!
//! Lowering settles each node's position once, as fir2ir passes the offsets of the FIR element it
//! converts to every IR element it builds for it: a node written explicitly, copied from another
//! node, or stamped with the position of the FIR expression or statement that built it is settled,
//! and a later stamp leaves it as it is. Settling is how lowering tells a node built without a
//! position from one not stamped yet; it is not a fact a later phase reads.

use std::ops::Range;

use super::expression_copy::InheritedFacts;
use super::{ExprId, IrFile};

/// The source lines of one expression node. `None` is a node without that position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrNodePosition {
    /// The line the expression's source text starts on, kotlinc's `startOffset` line.
    pub start_line: Option<u32>,
    /// The line the expression's source operation is on: where kotlinc marks it.
    pub source_line: Option<u32>,
    /// The line the expression's source text ends on.
    pub end_line: Option<u32>,
    /// The line the source statement this node roots begins on, where no node inside it carries
    /// that line of its own yet. kotlinc has no such fact: its statement's first instruction comes
    /// from a node that carries the statement's start offset itself. Once lowering gives every
    /// node its FIR expression's position this goes, and the JVM line rule reads node kinds only.
    pub statement_line: Option<u32>,
}

#[derive(Clone, Copy, Default)]
struct NodeSlot {
    position: IrNodePosition,
    settled: bool,
}

/// The positions of every expression node, by [`ExprId`].
#[derive(Default)]
pub(crate) struct IrNodePositions {
    nodes: Vec<NodeSlot>,
}

impl IrNodePositions {
    fn get(&self, expression: ExprId) -> IrNodePosition {
        self.nodes
            .get(expression as usize)
            .map(|slot| slot.position)
            .unwrap_or_default()
    }

    fn is_settled(&self, expression: ExprId) -> bool {
        self.nodes
            .get(expression as usize)
            .is_some_and(|slot| slot.settled)
    }

    /// The position of `expression`, settled because the caller is deciding it.
    fn settle(&mut self, expression: ExprId) -> &mut IrNodePosition {
        let index = expression as usize;
        if self.nodes.len() <= index {
            self.nodes.resize(index + 1, NodeSlot::default());
        }
        let slot = &mut self.nodes[index];
        slot.settled = true;
        &mut slot.position
    }

    /// The position of `expression`, which a statement line does not settle: the line a statement
    /// begins on is not the position of the node it roots.
    fn statement_mut(&mut self, expression: ExprId) -> &mut IrNodePosition {
        let index = expression as usize;
        if self.nodes.len() <= index {
            self.nodes.resize(index + 1, NodeSlot::default());
        }
        &mut self.nodes[index].position
    }
}

impl InheritedFacts for IrNodePositions {
    /// A copy is settled with the lines its source has, even when the source has none: copied code
    /// keeps its own position, never the position of the expression that copied it.
    fn inherit(&mut self, source: ExprId, target: ExprId) {
        let IrNodePosition {
            start_line,
            source_line,
            end_line,
            statement_line,
        } = self.get(source);
        let copy = self.settle(target);
        for (copied, line) in [
            (&mut copy.start_line, start_line),
            (&mut copy.source_line, source_line),
            (&mut copy.end_line, end_line),
        ] {
            if line.is_some() {
                *copied = line;
            }
        }
        if statement_line.is_some() {
            copy.statement_line = statement_line;
        }
    }
}

impl IrFile {
    /// Give `target` the source position `source` has.
    pub(crate) fn copy_position(&mut self, source: ExprId, target: ExprId) {
        self.positions.inherit(source, target);
    }

    /// Settle every node in `nodes` that is not settled yet with the start, source and end lines of
    /// `position`: the nodes lowering built for one FIR expression or statement take its position,
    /// and a node that an inner expression, an explicit write or a copy settled keeps its own.
    pub(crate) fn settle_positions(&mut self, nodes: Range<usize>, position: IrNodePosition) {
        for expression in nodes {
            let expression = expression as ExprId;
            if !self.positions.is_settled(expression) {
                let settled = self.positions.settle(expression);
                settled.start_line = position.start_line;
                settled.source_line = position.source_line;
                settled.end_line = position.end_line;
            }
        }
    }

    /// The source position of `expression`.
    pub fn position(&self, expression: ExprId) -> IrNodePosition {
        self.positions.get(expression)
    }

    /// The line `expression`'s source text starts on.
    pub fn start_line(&self, expression: ExprId) -> Option<u32> {
        self.positions.get(expression).start_line
    }

    /// The line `expression`'s source operation is on.
    pub fn source_line(&self, expression: ExprId) -> Option<u32> {
        self.positions.get(expression).source_line
    }

    /// The line `expression`'s source text ends on.
    pub fn end_line(&self, expression: ExprId) -> Option<u32> {
        self.positions.get(expression).end_line
    }

    pub fn set_source_line(&mut self, expression: ExprId, line: u32) {
        self.positions.settle(expression).source_line = Some(line);
    }

    pub fn set_end_line(&mut self, expression: ExprId, line: u32) {
        self.positions.settle(expression).end_line = Some(line);
    }

    /// Give `expression` the start line `line` unless it already has one.
    pub fn set_start_line_if_absent(&mut self, expression: ExprId, line: u32) {
        self.positions
            .settle(expression)
            .start_line
            .get_or_insert(line);
    }

    /// Give `expression` the source line `line` unless it already has one.
    pub fn set_source_line_if_absent(&mut self, expression: ExprId, line: u32) {
        self.positions
            .settle(expression)
            .source_line
            .get_or_insert(line);
    }

    /// Give `expression` the end line `line` unless it already has one.
    pub fn set_end_line_if_absent(&mut self, expression: ExprId, line: u32) {
        self.positions
            .settle(expression)
            .end_line
            .get_or_insert(line);
    }

    /// Remove `expression`'s source line, returning it.
    pub fn take_source_line(&mut self, expression: ExprId) -> Option<u32> {
        self.positions.settle(expression).source_line.take()
    }

    /// The line the source statement `expression` roots begins on.
    pub fn statement_line(&self, expression: ExprId) -> Option<u32> {
        self.positions.get(expression).statement_line
    }

    pub fn set_statement_line(&mut self, expression: ExprId, line: u32) {
        self.positions.statement_mut(expression).statement_line = Some(line);
    }

    /// Remove `expression`'s statement line, returning it.
    pub fn take_statement_line(&mut self, expression: ExprId) -> Option<u32> {
        self.positions
            .statement_mut(expression)
            .statement_line
            .take()
    }

    /// Remove `expression`'s end line, returning it.
    pub fn take_end_line(&mut self, expression: ExprId) -> Option<u32> {
        self.positions.settle(expression).end_line.take()
    }
}

#[cfg(test)]
mod tests {
    use super::IrNodePosition;
    use crate::ir::{IrConst, IrExpr, IrFile};

    fn position(line: u32) -> IrNodePosition {
        IrNodePosition {
            start_line: Some(line),
            source_line: Some(line),
            end_line: Some(line + 1),
            statement_line: None,
        }
    }

    fn constant(ir: &mut IrFile) -> usize {
        ir.add_expr(IrExpr::Const(IrConst::Int(0))) as usize
    }

    #[test]
    fn a_stamp_settles_only_the_nodes_nothing_settled_before() {
        let mut ir = IrFile::default();
        let explicit = constant(&mut ir);
        ir.set_source_line(explicit as u32, 2);
        let positionless = constant(&mut ir);
        ir.settle_positions(positionless..positionless + 1, IrNodePosition::default());
        let built = constant(&mut ir);

        ir.settle_positions(explicit..ir.exprs.len(), position(7));

        assert_eq!(
            ir.position(explicit as u32),
            IrNodePosition {
                start_line: None,
                source_line: Some(2),
                end_line: None,
                statement_line: None,
            }
        );
        assert_eq!(ir.position(positionless as u32), IrNodePosition::default());
        assert_eq!(ir.position(built as u32), position(7));
    }

    #[test]
    fn a_copy_of_a_node_without_a_position_is_not_stamped() {
        let mut ir = IrFile::default();
        let source = constant(&mut ir);
        ir.settle_positions(source..source + 1, IrNodePosition::default());
        let copy = constant(&mut ir);
        ir.copy_position(source as u32, copy as u32);

        ir.settle_positions(copy..ir.exprs.len(), position(7));

        assert_eq!(ir.position(copy as u32), IrNodePosition::default());
    }
}
