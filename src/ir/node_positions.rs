//! The source position every expression node carries, kotlinc's `IrElement` offsets in line form.
//!
//! A position belongs to the node, not to a table a pass must remember: it lives in a column
//! parallel to [`IrFile::exprs`], so a copy of the node takes it with the node's other facts
//! ([`InheritedFacts`]). A node built without a source location has none.

use super::expression_copy::InheritedFacts;
use super::{ExprId, IrFile};

/// The source lines of one expression node. `None` is a node without that position.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IrNodePosition {
    /// The line the expression's source operation is on: where kotlinc marks it.
    pub source_line: Option<u32>,
    /// The line the expression's source text ends on.
    pub end_line: Option<u32>,
}

/// The positions of every expression node, by [`ExprId`].
#[derive(Default)]
pub(crate) struct IrNodePositions {
    nodes: Vec<IrNodePosition>,
}

impl IrNodePositions {
    fn get(&self, expression: ExprId) -> IrNodePosition {
        self.nodes
            .get(expression as usize)
            .copied()
            .unwrap_or_default()
    }

    fn get_mut(&mut self, expression: ExprId) -> &mut IrNodePosition {
        let index = expression as usize;
        if self.nodes.len() <= index {
            self.nodes.resize(index + 1, IrNodePosition::default());
        }
        &mut self.nodes[index]
    }
}

impl InheritedFacts for IrNodePositions {
    fn inherit(&mut self, source: ExprId, target: ExprId) {
        let IrNodePosition {
            source_line,
            end_line,
        } = self.get(source);
        let copy = self.get_mut(target);
        if source_line.is_some() {
            copy.source_line = source_line;
        }
        if end_line.is_some() {
            copy.end_line = end_line;
        }
    }
}

impl IrFile {
    /// Give `target` the source position `source` has.
    pub(crate) fn copy_position(&mut self, source: ExprId, target: ExprId) {
        self.positions.inherit(source, target);
    }

    /// The source position of `expression`.
    pub fn position(&self, expression: ExprId) -> IrNodePosition {
        self.positions.get(expression)
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
        self.positions.get_mut(expression).source_line = Some(line);
    }

    pub fn set_end_line(&mut self, expression: ExprId, line: u32) {
        self.positions.get_mut(expression).end_line = Some(line);
    }

    /// Give `expression` the source line `line` unless it already has one.
    pub fn set_source_line_if_absent(&mut self, expression: ExprId, line: u32) {
        self.positions
            .get_mut(expression)
            .source_line
            .get_or_insert(line);
    }

    /// Give `expression` the end line `line` unless it already has one.
    pub fn set_end_line_if_absent(&mut self, expression: ExprId, line: u32) {
        self.positions
            .get_mut(expression)
            .end_line
            .get_or_insert(line);
    }

    /// Remove `expression`'s source line, returning it.
    pub fn take_source_line(&mut self, expression: ExprId) -> Option<u32> {
        self.positions.get_mut(expression).source_line.take()
    }

    /// Remove `expression`'s end line, returning it.
    pub fn take_end_line(&mut self, expression: ExprId) -> Option<u32> {
        self.positions.get_mut(expression).end_line.take()
    }
}
