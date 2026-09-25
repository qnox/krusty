//! kotlinc's backward variable liveness (`codegen/optimization/common/variableLiveness.kt` over
//! `backwardAnalysis.kt`): which local slots are read before being written on some path from each
//! node. A `long`/`double` store kills only its first slot, exactly as kotlinc's does.

use super::super::opcodes::*;
use super::control_flow::ControlFlowGraph;
use crate::jvm::method_node::{Insn, MethodNode, Node};

/// Liveness at one node: the slots alive BEFORE it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct VariableLiveness {
    alive: Vec<bool>,
}

impl VariableLiveness {
    fn new(max_locals: usize) -> Self {
        VariableLiveness {
            alive: vec![false; max_locals],
        }
    }

    pub(crate) fn is_alive(&self, slot: usize) -> bool {
        self.alive.get(slot).copied().unwrap_or(false)
    }

    fn set(&mut self, slot: usize, alive: bool) {
        if slot >= self.alive.len() {
            self.alive.resize(slot + 1, false);
        }
        self.alive[slot] = alive;
    }
}

/// `analyzeLiveness`: one frame per node, iterated to a fixed point in reverse node order.
pub(crate) fn analyze_liveness(method: &MethodNode) -> Vec<VariableLiveness> {
    let graph = ControlFlowGraph::build(method, true);
    let max_locals = usize::from(method.max_locals);
    let mut frames = vec![VariableLiveness::new(max_locals); method.nodes.len()];
    loop {
        let mut changed = false;
        for index in (0..method.nodes.len()).rev() {
            let mut frame = VariableLiveness::new(max_locals);
            for &successor in graph.successors(index) {
                for (slot, alive) in frames[successor].alive.iter().enumerate() {
                    if *alive {
                        frame.set(slot, true);
                    }
                }
            }
            match &method.nodes[index] {
                Node::Insn(Insn::Var { op, slot }) => {
                    frame.set(usize::from(*slot), !(ISTORE..=ASTORE).contains(op));
                }
                Node::Insn(Insn::Iinc { slot, .. }) => frame.set(usize::from(*slot), true),
                _ => {}
            }
            if frames[index] != frame {
                frames[index] = frame;
                changed = true;
            }
        }
        if !changed {
            return frames;
        }
    }
}
