//! kotlinc's `ControlFlowGraph` (`codegen/optimization/common/ControlFlowGraph.kt`): the edges
//! between the nodes of a [`MethodNode`], indexed by node position, labels and line numbers
//! included — they are nodes with a fall-through edge, exactly as in ASM's instruction list.

use std::collections::BTreeSet;

use crate::jvm::method_node::LabelPositions;
use crate::jvm::method_node::{MethodNode, Node};

pub(crate) struct ControlFlowGraph {
    successors: Vec<Vec<usize>>,
    predecessors: Vec<Vec<usize>>,
}

impl ControlFlowGraph {
    /// The graph of `method`. With `follow_exceptions` false, an exception edge still makes its
    /// handler reachable but is not recorded as an edge.
    pub(crate) fn build(method: &MethodNode, follow_exceptions: bool) -> Self {
        let positions = LabelPositions::of(method);
        let count = method.nodes.len();
        let mut handlers: Vec<Vec<usize>> = vec![Vec::new(); count];
        for block in &method.try_catch_blocks {
            let handler = positions.at(block.handler);
            for entry in &mut handlers[positions.at(block.start)..positions.at(block.end)] {
                entry.push(handler);
            }
        }
        let mut predecessors: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); count];
        let mut queued = vec![false; count];
        let mut queue = Vec::new();
        if count > 0 {
            queued[0] = true;
            queue.push(0);
        }
        let mut edge = |from: usize, to: usize, record: bool, queue: &mut Vec<usize>| {
            if record {
                predecessors[to].insert(from);
            }
            if !queued[to] {
                queued[to] = true;
                queue.push(to);
            }
        };
        while let Some(index) = queue.pop() {
            match &method.nodes[index] {
                Node::Label(_) | Node::Line { .. } => edge(index, index + 1, true, &mut queue),
                Node::Insn(insn) => {
                    if insn.falls_through() {
                        edge(index, index + 1, true, &mut queue);
                    }
                    for target in insn.jump_targets() {
                        edge(index, positions.at(target), true, &mut queue);
                    }
                }
            }
            for &handler in &handlers[index] {
                edge(index, handler, follow_exceptions, &mut queue);
            }
        }
        let mut successors = vec![Vec::new(); count];
        let predecessors: Vec<Vec<usize>> = predecessors
            .into_iter()
            .map(|set| set.into_iter().collect())
            .collect();
        for (to, froms) in predecessors.iter().enumerate() {
            for &from in froms {
                successors[from].push(to);
            }
        }
        ControlFlowGraph {
            successors,
            predecessors,
        }
    }

    pub(crate) fn successors(&self, index: usize) -> &[usize] {
        &self.successors[index]
    }

    pub(crate) fn predecessors(&self, index: usize) -> &[usize] {
        &self.predecessors[index]
    }
}
