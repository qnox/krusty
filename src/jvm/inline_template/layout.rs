//! Where a method's blocks start, and what each label and node stands for in class-file order.
//!
//! A block starts at the first instruction of the method, at the first instruction at or after a
//! label a jump, switch or exception-table entry names, and after every instruction that does not
//! simply fall through. Protected-range boundaries start a block too, so every block lies wholly
//! inside or wholly outside each range. Labels and line numbers are not instructions: a label
//! stands for the next instruction after it, and a run of labels shares one block.

use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

use super::DecodeError;

pub(super) struct Layout {
    /// The node index each label is placed at, by label index.
    label_nodes: Vec<Option<usize>>,
    /// For each node, the first instruction node at or after it; `None` past the last one.
    next_instruction: Vec<Option<usize>>,
    /// Whether an instruction node starts a block.
    starts_block: Vec<bool>,
    /// The line in effect at each node in class-file order.
    lines: Vec<Option<u16>>,
}

impl Layout {
    pub(super) fn of(method: &MethodNode) -> Result<Layout, DecodeError> {
        let count = method.nodes.len();
        let mut label_nodes = vec![None; method.label_count()];
        for (index, node) in method.nodes.iter().enumerate() {
            if let Node::Label(label) = node {
                let slot = label_nodes.get_mut(label.index()).ok_or_else(|| {
                    DecodeError::malformed(index, "a label the method never handed out")
                })?;
                *slot = Some(index);
            }
        }

        let mut next_instruction = vec![None; count];
        let mut next = None;
        for index in (0..count).rev() {
            if matches!(method.nodes[index], Node::Insn(_)) {
                next = Some(index);
            }
            next_instruction[index] = next;
        }

        let mut layout = Layout {
            label_nodes,
            next_instruction,
            starts_block: vec![false; count],
            lines: Vec::with_capacity(count),
        };
        let mut line = None;
        for node in &method.nodes {
            if let Node::Line { line: number, .. } = node {
                line = Some(*number);
            }
            layout.lines.push(line);
        }

        if let Some(first) = layout.next_instruction.first().copied().flatten() {
            layout.starts_block[first] = true;
        }
        let mut leaders = Vec::new();
        for (index, node) in method.nodes.iter().enumerate() {
            let Node::Insn(insn) = node else { continue };
            leaders.extend(insn.jump_targets());
            if ends_block(insn) {
                if let Some(after) = layout.next_instruction.get(index + 1).copied().flatten() {
                    layout.starts_block[after] = true;
                }
            }
        }
        for block in &method.try_catch_blocks {
            leaders.extend([block.start, block.end, block.handler]);
        }
        for label in leaders {
            let node = layout.label_node(label)?;
            if let Some(instruction) = layout.next_instruction[node] {
                layout.starts_block[instruction] = true;
            }
        }
        Ok(layout)
    }

    /// The node index `label` is placed at.
    pub(super) fn label_node(&self, label: LabelId) -> Result<usize, DecodeError> {
        self.label_nodes
            .get(label.index())
            .copied()
            .flatten()
            .ok_or(DecodeError::malformed(
                0,
                "a label the method refers to is not placed",
            ))
    }

    /// The instruction `label` stands for, `None` when only labels and lines follow it.
    pub(super) fn label_instruction(&self, label: LabelId) -> Result<Option<usize>, DecodeError> {
        Ok(self.next_instruction[self.label_node(label)?])
    }

    /// The block starts, as instruction node indices in class-file order.
    pub(super) fn block_starts(&self) -> impl Iterator<Item = usize> + '_ {
        self.starts_block
            .iter()
            .enumerate()
            .filter_map(|(index, starts)| starts.then_some(index))
    }

    pub(super) fn line(&self, index: usize) -> Option<u16> {
        self.lines[index]
    }
}

/// Whether control can leave the instruction other than by falling through to the next one.
fn ends_block(insn: &Insn) -> bool {
    !insn.falls_through() || !insn.jump_targets().is_empty()
}
