//! The inlined body's returns: kotlinc's `LocalReturnsNormalizer`, which leaves each return with
//! only its value on the stack, and `processReturns`, which turns each into a jump to the end of
//! the inlined code.

use crate::jvm::method_node::{stack_shapes, Category, Insn, LabelId, MethodNode, Node};

use super::InlineError;

const LRETURN: u8 = 0xad;
const DRETURN: u8 = 0xaf;
const RETURN: u8 = 0xb1;
const GOTO: u8 = 0xa7;
const NOP: u8 = 0x00;

fn is_return(insn: &Insn) -> bool {
    matches!(insn, Insn::Op(0xac..=0xb1))
}

/// Before a return that finds more than its value on the stack, store the value to a fresh local,
/// pop the rest and load it back: the inlined code continues at one point whatever each return
/// left under it. The local is the body's first free slot, taken once for every such return.
pub(super) fn normalize_local_returns(node: &mut MethodNode) -> Result<(), InlineError> {
    let shapes = stack_shapes(node).map_err(InlineError::Stack)?;
    let mut returns = Vec::new();
    let mut opcode = None;
    for (at, entry) in node.nodes.iter().enumerate() {
        let (Node::Insn(insn @ Insn::Op(op)), Some(stack)) = (entry, &shapes[at]) else {
            continue;
        };
        if !is_return(insn) {
            continue;
        }
        if opcode.is_some_and(|known| known != *op) {
            return Err(InlineError::MixedReturns);
        }
        opcode = Some(*op);
        returns.push((at, stack.clone()));
    }
    let Some(opcode) = opcode else {
        return Ok(());
    };
    let with_value = opcode != RETURN;
    let variable = if with_value {
        let variable = node.max_locals;
        node.max_locals += if matches!(opcode, LRETURN | DRETURN) {
            2
        } else {
            1
        };
        variable
    } else {
        0
    };
    for (at, stack) in returns.into_iter().rev() {
        let expected = usize::from(with_value);
        if stack.len() == expected {
            continue;
        }
        let mut inserted = Vec::new();
        let mut remaining: &[Category] = &stack;
        let top = *stack
            .last()
            .expect("a return with a value finds one on the stack");
        if with_value {
            inserted.push(Node::Insn(Insn::Var {
                op: top.store_op(),
                slot: variable,
            }));
            remaining = &stack[..stack.len() - 1];
        }
        for value in remaining.iter().rev() {
            inserted.push(Node::Insn(Insn::Op(value.pop_op())));
        }
        if with_value {
            inserted.push(Node::Insn(Insn::Var {
                op: top.load_op(),
                slot: variable,
            }));
        }
        node.nodes.splice(at..at, inserted);
    }
    Ok(())
}

/// Replace every return with `nop; goto end` and place a fresh label after the jump.
pub(super) fn process_returns(node: &mut MethodNode, end: LabelId) {
    let mut at = 0;
    while at < node.nodes.len() {
        if matches!(&node.nodes[at], Node::Insn(insn) if is_return(insn)) {
            let after = node.new_label();
            node.nodes.splice(
                at..=at,
                [
                    Node::Insn(Insn::Op(NOP)),
                    Node::Insn(Insn::Jump {
                        op: GOTO,
                        target: end,
                    }),
                    Node::Label(after),
                ],
            );
            at += 3;
        } else {
            at += 1;
        }
    }
}
