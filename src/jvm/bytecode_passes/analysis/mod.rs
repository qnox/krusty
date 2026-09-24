//! Dataflow analyses over a [`MethodNode`], ported from the ones kotlinc's bytecode passes use
//! (`codegen/optimization/common/`). Every analysis is indexed by node position — labels and line
//! numbers are nodes, as in ASM's `InsnList`, so a port's index arithmetic stays kotlinc's.

mod basic_values;
mod control_flow;
mod fast_analyzer;
mod frame;
mod liveness;

pub(crate) use basic_values::{BasicInterpreter, BasicValue};
pub(crate) use control_flow::ControlFlowGraph;
pub(crate) use fast_analyzer::{analyze, analyze_with, AnalyzerError, AnalyzerOptions, Executor};
pub(crate) use frame::{At, Frame, Interpreter, Value};
pub(crate) use liveness::{analyze_liveness, VariableLiveness};

use std::collections::HashMap;

use super::opcodes::*;
use crate::jvm::method_node::{Insn, LabelId, MethodNode, Node};

/// Computes the maximum reachable operand-stack depth in JVM words from the method's final body.
/// This is representation bookkeeping: transformations do not estimate how much stack the code
/// they insert might need.
pub(crate) fn computed_max_stack(method: &MethodNode, owner: &str) -> Result<u16, AnalyzerError> {
    let frames = analyze(method, owner, &mut BasicInterpreter)?;
    let words = frames
        .iter()
        .flatten()
        .map(|frame| frame.stack.iter().map(Value::size).sum::<usize>())
        .max()
        .unwrap_or(0);
    u16::try_from(words).map_err(|_| AnalyzerError {
        index: 0,
        message: "operand stack exceeds the classfile limit".to_string(),
    })
}

/// Where each label of a method stands.
pub(crate) struct Positions(HashMap<LabelId, usize>);

impl Positions {
    pub(crate) fn of(method: &MethodNode) -> Self {
        Positions(
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
    pub(crate) fn at(&self, label: LabelId) -> usize {
        self.0[&label]
    }
}

/// ASM's `AbstractInsnNode.getOpcode` of an instruction.
pub(crate) fn opcode(insn: &Insn) -> u8 {
    match insn {
        Insn::Op(op)
        | Insn::Int { op, .. }
        | Insn::Var { op, .. }
        | Insn::Type { op, .. }
        | Insn::Field { op, .. }
        | Insn::Method { op, .. }
        | Insn::Jump { op, .. } => *op,
        Insn::Iinc { .. } => IINC,
        Insn::InvokeDynamic { .. } => INVOKEDYNAMIC,
        Insn::Ldc(_) => LDC,
        Insn::TableSwitch { .. } => TABLESWITCH,
        Insn::LookupSwitch { .. } => LOOKUPSWITCH,
        Insn::MultiANewArray { .. } => MULTIANEWARRAY,
    }
}

/// The node's opcode, `None` for a label or line number (ASM's `-1`).
pub(crate) fn node_opcode(node: &Node) -> Option<u8> {
    match node {
        Node::Insn(insn) => Some(opcode(insn)),
        _ => None,
    }
}

/// The labels a jump or switch can transfer to, default first, in the instruction's order.
pub(crate) fn jump_targets(insn: &Insn) -> Vec<LabelId> {
    match insn {
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

/// Whether control can fall through `insn` to the next node.
pub(crate) fn falls_through(insn: &Insn) -> bool {
    let op = opcode(insn);
    !matches!(insn, Insn::TableSwitch { .. } | Insn::LookupSwitch { .. })
        && op != GOTO
        && op != JSR
        && op != ATHROW
        && !(IRETURN..=RETURN).contains(&op)
}

/// kotlinc's `isMeaningful`: an instruction, `nop` included, rather than a label or line number.
pub(crate) fn is_meaningful(node: &Node) -> bool {
    matches!(node, Node::Insn(_))
}

/// kotlinc's `isBranchOrCall`: a jump, a switch or a method call (not `invokedynamic`).
pub(crate) fn is_branch_or_call(node: &Node) -> bool {
    matches!(
        node,
        Node::Insn(
            Insn::Jump { .. }
                | Insn::TableSwitch { .. }
                | Insn::LookupSwitch { .. }
                | Insn::Method { .. }
        )
    )
}

/// The slot a store (`istore`…`astore`) writes.
pub(crate) fn stored_slot(node: &Node) -> Option<u16> {
    match node {
        Node::Insn(Insn::Var { op, slot }) if (ISTORE..=ASTORE).contains(op) => Some(*slot),
        _ => None,
    }
}
