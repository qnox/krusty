//! The control flow between the instructions of a [`MethodNode`], with normal and exceptional
//! edges kept apart.
//!
//! The dataflow analyses of the redundant-null-check and temporary-variable passes keep one state
//! before each instruction; labels and line numbers carry no state of their own, so this graph
//! numbers the instructions alone, in list order, with one virtual exit past the last. A label
//! stands in front of the instruction that follows it.
//!
//! Normal and exceptional edges are separate because a store does not happen along an exceptional
//! edge that leaves the storing instruction: the handler sees the locals as they stood before it
//! as well as after.

use std::collections::HashMap;

use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

const GOTO: u8 = 0xa7;
const JSR: u8 = 0xa8;
const RET: u8 = 0xa9;

pub(crate) struct InstructionGraph<'a> {
    method: &'a MethodNode,
    /// The node position of each instruction.
    positions: Vec<usize>,
    /// The instruction each label stands in front of; the exit for a label after the last one.
    label_at: HashMap<LabelId, usize>,
    normal: Vec<Vec<usize>>,
    exceptional: Vec<Vec<usize>>,
}

impl<'a> InstructionGraph<'a> {
    /// The graph of `method`, or `None` when it uses the subroutine instructions (`jsr`/`ret`),
    /// whose successors this graph cannot express; kotlinc never emits them. A protected range
    /// that covers no instruction adds no edge.
    pub(crate) fn build(method: &'a MethodNode) -> Option<InstructionGraph<'a>> {
        let mut positions = Vec::new();
        let mut label_at = HashMap::new();
        let mut pending = Vec::new();
        for (at, node) in method.nodes.iter().enumerate() {
            match node {
                Node::Label(label) => pending.push(*label),
                Node::Line { .. } => {}
                Node::Insn(_) => {
                    for label in pending.drain(..) {
                        label_at.insert(label, positions.len());
                    }
                    positions.push(at);
                }
            }
        }
        let exit = positions.len();
        for label in pending {
            label_at.insert(label, exit);
        }
        let ordinal = |label: &LabelId| label_at.get(label).copied();
        let mut normal = vec![Vec::new(); exit + 1];
        let mut exceptional = vec![Vec::new(); exit + 1];
        for (index, &at) in positions.iter().enumerate() {
            let Node::Insn(insn) = &method.nodes[at] else {
                unreachable!("positions name instructions");
            };
            match insn {
                Insn::Var { op: RET, .. } | Insn::Jump { op: JSR, .. } => return None,
                Insn::Jump { op, target } => {
                    normal[index].push(ordinal(target)?);
                    if *op != GOTO {
                        normal[index].push(index + 1);
                    }
                }
                Insn::TableSwitch { .. } | Insn::LookupSwitch { .. } => {
                    for target in insn.jump_targets() {
                        normal[index].push(ordinal(&target)?);
                    }
                }
                insn if insn.falls_through() => normal[index].push(index + 1),
                _ => {}
            }
        }
        for block in &method.try_catch_blocks {
            let (start, end, handler) = (
                ordinal(&block.start)?,
                ordinal(&block.end)?,
                ordinal(&block.handler)?,
            );
            if handler >= exit {
                return None;
            }
            for protected in exceptional.iter_mut().take(end).skip(start) {
                if !protected.contains(&handler) {
                    protected.push(handler);
                }
            }
        }
        Some(InstructionGraph {
            method,
            positions,
            label_at,
            normal,
            exceptional,
        })
    }

    /// How many instructions the method has: the exit's number.
    pub(crate) fn len(&self) -> usize {
        self.positions.len()
    }

    /// Instruction `index`.
    pub(crate) fn insn(&self, index: usize) -> &'a Insn {
        match &self.method.nodes[self.positions[index]] {
            Node::Insn(insn) => insn,
            _ => unreachable!("positions name instructions"),
        }
    }

    /// The instruction `label` stands in front of, the exit when it stands after the last one.
    pub(crate) fn at_label(&self, label: LabelId) -> usize {
        self.label_at[&label]
    }

    /// The labels standing in front of instruction `index` (the exit for `len()`), after the
    /// instruction before it.
    pub(crate) fn labels_before(&self, index: usize) -> impl Iterator<Item = LabelId> + 'a {
        let from = index
            .checked_sub(1)
            .map_or(0, |previous| self.positions[previous] + 1);
        let to = self
            .positions
            .get(index)
            .copied()
            .unwrap_or(self.method.nodes.len());
        self.method.nodes[from..to]
            .iter()
            .filter_map(|node| match node {
                Node::Label(label) => Some(*label),
                _ => None,
            })
    }

    pub(crate) fn normal_successors(&self, index: usize) -> &[usize] {
        self.normal.get(index).map(Vec::as_slice).unwrap_or(&[])
    }

    pub(crate) fn exceptional_successors(&self, index: usize) -> &[usize] {
        self.exceptional
            .get(index)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The instructions reachable from the first, in reverse post-order: the visit order in which a
    /// forward dataflow converges in few sweeps.
    pub(crate) fn reverse_post_order(&self) -> Vec<usize> {
        let exit = self.len();
        let mut order = Vec::new();
        if exit == 0 {
            return order;
        }
        let mut state = vec![0u8; exit + 1]; // 0 unseen, 1 on the stack, 2 done
        let mut stack = vec![(0usize, 0usize)];
        state[0] = 1;
        while let Some((index, next)) = stack.pop() {
            let mut successors = self
                .normal_successors(index)
                .iter()
                .chain(self.exceptional_successors(index));
            match successors.nth(next) {
                Some(&to) => {
                    stack.push((index, next + 1));
                    if state[to] == 0 {
                        state[to] = 1;
                        stack.push((to, 0));
                    }
                }
                None => {
                    state[index] = 2;
                    order.push(index);
                }
            }
        }
        order.reverse();
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::method_node::TryCatchBlock;

    const NOP: u8 = 0x00;
    const IFEQ: u8 = 0x99;
    const RETURN: u8 = 0xb1;
    const ATHROW: u8 = 0xbf;

    /// A method of `insns` with a label in front of each and one after the last; `Err((op, k))`
    /// jumps to label `k`.
    fn method(insns: &[Result<u8, (u8, usize)>]) -> (MethodNode, Vec<LabelId>) {
        let mut method = MethodNode::new(0x0009, "f", "()V");
        let labels: Vec<LabelId> = (0..=insns.len()).map(|_| method.new_label()).collect();
        for (k, insn) in insns.iter().enumerate() {
            method.nodes.push(Node::Label(labels[k]));
            method.nodes.push(Node::Insn(match *insn {
                Ok(op) => Insn::Op(op),
                Err((op, to)) => Insn::Jump {
                    op,
                    target: labels[to],
                },
            }));
        }
        method.nodes.push(Node::Label(labels[insns.len()]));
        (method, labels)
    }

    #[test]
    fn straight_line_code_falls_through_and_a_terminator_does_not() {
        let (method, _) = method(&[Ok(NOP), Ok(ATHROW), Ok(RETURN)]);
        let graph = InstructionGraph::build(&method).expect("graph");
        assert_eq!(graph.normal_successors(0), [1]);
        assert_eq!(graph.normal_successors(1), [] as [usize; 0]);
        assert_eq!(graph.normal_successors(2), [] as [usize; 0]);
    }

    #[test]
    fn a_conditional_branches_and_falls_through_but_a_goto_only_branches() {
        let (method, _) = method(&[Err((IFEQ, 3)), Err((GOTO, 3)), Ok(NOP), Ok(RETURN)]);
        let graph = InstructionGraph::build(&method).expect("graph");
        assert_eq!(graph.normal_successors(0), [3, 1]);
        assert_eq!(graph.normal_successors(1), [3]);
    }

    #[test]
    fn a_switch_reaches_its_default_and_every_arm() {
        let (mut method, labels) = method(&[Ok(NOP), Ok(RETURN), Ok(RETURN), Ok(RETURN)]);
        method.nodes[1] = Node::Insn(Insn::TableSwitch {
            low: 0,
            high: 1,
            default: labels[3],
            labels: vec![labels[1], labels[2]],
        });
        let graph = InstructionGraph::build(&method).expect("graph");
        assert_eq!(graph.normal_successors(0), [3, 1, 2]);
    }

    #[test]
    fn a_protected_range_reaches_its_handler_from_every_instruction_in_it() {
        let (mut method, labels) = method(&[Ok(NOP), Ok(NOP), Ok(NOP), Ok(RETURN)]);
        method.try_catch_blocks.push(TryCatchBlock {
            start: labels[1],
            end: labels[3],
            handler: labels[3],
            catch_type: None,
        });
        let graph = InstructionGraph::build(&method).expect("graph");
        assert_eq!(graph.exceptional_successors(0), [] as [usize; 0]);
        assert_eq!(graph.exceptional_successors(1), [3]);
        assert_eq!(graph.exceptional_successors(2), [3]);
        assert_eq!(graph.exceptional_successors(3), [] as [usize; 0]);
    }

    #[test]
    fn labels_stand_in_front_of_the_next_instruction() {
        let (method, labels) = method(&[Ok(NOP), Ok(RETURN)]);
        let graph = InstructionGraph::build(&method).expect("graph");
        assert_eq!(graph.at_label(labels[1]), 1);
        assert_eq!(graph.at_label(labels[2]), 2);
        assert_eq!(graph.labels_before(1).collect::<Vec<_>>(), [labels[1]]);
        assert_eq!(graph.labels_before(2).collect::<Vec<_>>(), [labels[2]]);
    }

    #[test]
    fn the_subroutine_instructions_are_refused() {
        let (method, _) = method(&[Err((JSR, 1)), Ok(RETURN)]);
        assert!(InstructionGraph::build(&method).is_none());
    }

    #[test]
    fn reverse_post_order_visits_a_block_before_its_successors() {
        let (method, _) = method(&[Err((IFEQ, 3)), Ok(NOP), Err((GOTO, 4)), Ok(NOP), Ok(RETURN)]);
        let graph = InstructionGraph::build(&method).expect("graph");
        let order = graph.reverse_post_order();
        let position = |index: usize| order.iter().position(|&i| i == index).expect("reached");
        assert!(position(0) < position(1));
        assert!(position(0) < position(3));
        assert!(position(1) < position(2));
        assert!(position(2) < position(4));
    }
}
