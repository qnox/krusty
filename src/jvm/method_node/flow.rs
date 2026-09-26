//! What an instruction does to control flow, and where a method's labels stand: the facts every
//! pass over a [`MethodNode`] walks the body by.

use std::collections::{BTreeSet, HashMap};

use super::nodes::{Insn, LabelId, MethodNode, Node};

const GOTO: u8 = 0xa7;
const JSR: u8 = 0xa8;
const ATHROW: u8 = 0xbf;
const IRETURN: u8 = 0xac;
const RETURN: u8 = 0xb1;

impl Insn {
    /// The labels a jump or switch can transfer to, default first, in the instruction's order.
    pub fn jump_targets(&self) -> Vec<LabelId> {
        match self {
            Insn::Jump { target, .. } => vec![*target],
            Insn::TableSwitch {
                default, labels, ..
            }
            | Insn::LookupSwitch {
                default, labels, ..
            } => std::iter::once(*default)
                .chain(labels.iter().copied())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether control can fall through the instruction to the next node.
    pub fn falls_through(&self) -> bool {
        match *self {
            Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => false,
            Insn::Jump { op, .. } => op != GOTO && op != JSR,
            Insn::Op(op) => op != ATHROW && !(IRETURN..=RETURN).contains(&op),
            _ => true,
        }
    }
}

/// Where each label of a method stands.
pub struct LabelPositions(HashMap<LabelId, usize>);

impl LabelPositions {
    pub fn of(method: &MethodNode) -> Self {
        LabelPositions(
            method
                .nodes
                .iter()
                .enumerate()
                .filter_map(|(index, node)| match node {
                    Node::Label(label) => Some((*label, index)),
                    _ => None,
                })
                .collect(),
        )
    }

    /// The node index of `label`. A label a method refers to but does not contain is a malformed
    /// body, which no pass can recover from.
    pub fn at(&self, label: LabelId) -> usize {
        self.0[&label]
    }
}

impl MethodNode {
    /// Every label something in the method refers to: a jump or switch, a line number, a protected
    /// range or a local variable's range. ASM only creates such labels, so a label outside this set
    /// separates no two nodes in the list kotlinc's passes see.
    pub(crate) fn referenced_labels(&self) -> BTreeSet<LabelId> {
        let mut labels = BTreeSet::new();
        for node in &self.nodes {
            match node {
                Node::Insn(insn) => labels.extend(insn.jump_targets()),
                Node::Line { start, .. } => {
                    labels.insert(*start);
                }
                Node::Label(_) => {}
            }
        }
        for block in &self.try_catch_blocks {
            labels.extend([block.start, block.end, block.handler]);
        }
        for local in &self.local_variables {
            labels.extend([local.start, local.end]);
        }
        labels
    }
}
