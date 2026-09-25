//! Test bodies for the jump and `nop` passes: a [`MethodNode`] with a label in front of every
//! instruction and one past the end, so a test names a jump target, a line start or a local's range
//! by instruction number.

use crate::jvm::method_node::{Insn, LabelId, LocalVariable, MethodNode, Node};

/// A body built by [`labelled_body`], with the label of every instruction.
pub(crate) struct LabelledBody {
    pub method: MethodNode,
    pub labels: Vec<LabelId>,
}

/// A body of `insns`, where `Err((op, k))` jumps to label `k`. A line starts at each instruction
/// `lines` names; `bounds` is one local's range.
pub(crate) fn labelled_body(
    insns: &[Result<u8, (u8, usize)>],
    lines: &[usize],
    bounds: Option<(usize, usize)>,
) -> LabelledBody {
    let mut method = MethodNode::new(0x0009, "f", "()I");
    let labels: Vec<LabelId> = (0..=insns.len()).map(|_| method.new_label()).collect();
    for (k, insn) in insns.iter().enumerate() {
        method.nodes.push(Node::Label(labels[k]));
        if lines.contains(&k) {
            method.nodes.push(Node::Line {
                line: k as u16 + 1,
                start: labels[k],
            });
        }
        method.nodes.push(Node::Insn(match *insn {
            Ok(op) => Insn::Op(op),
            Err((op, to)) => Insn::Jump {
                op,
                target: labels[to],
            },
        }));
    }
    method.nodes.push(Node::Label(labels[insns.len()]));
    if let Some((start, end)) = bounds {
        method.local_variables.push(LocalVariable {
            name: "x".to_string(),
            desc: "I".to_string(),
            start: labels[start],
            end: labels[end],
            slot: 0,
        });
    }
    LabelledBody { method, labels }
}

impl LabelledBody {
    /// A jump with `op` to label `to`.
    pub fn jump(&self, op: u8, to: usize) -> Insn {
        Insn::Jump {
            op,
            target: self.labels[to],
        }
    }
}

/// Instructions without operands.
pub(crate) fn ops(ops: &[u8]) -> Vec<Insn> {
    ops.iter().map(|&op| Insn::Op(op)).collect()
}
