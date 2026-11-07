//! The inlined body's returns: kotlinc's `LocalReturnsNormalizer`, which leaves each return with
//! only its value on the stack, and `processReturns`, which turns each into a jump to the end of
//! the inlined code.
//!
//! A lambda's non-local `return` leaves the function the lambda is written in, not the inlined
//! one. kotlinc marks it with a call to `$$$$$NON_LOCAL_RETURN$$$$$` just before the return
//! (`generateGlobalReturnFlag`), and both passes leave a marked return as it is: it stays a return
//! of the caller, in front of which the inlined function's `finally` blocks are copied
//! (`finally_blocks`), and the marker goes when the code is written into its caller.

use crate::jvm::method_node::{stack_shapes, Category, Insn, LabelId, MethodNode, Node};

use super::InlineError;

const LRETURN: u8 = 0xad;
const DRETURN: u8 = 0xaf;
const RETURN: u8 = 0xb1;
const GOTO: u8 = 0xa7;
const NOP: u8 = 0x00;

const INVOKESTATIC: u8 = 0xb8;
/// kotlinc's `NON_LOCAL_RETURN`, the owner of the marker call in front of a non-local return.
const NON_LOCAL_RETURN: &str = "$$$$$NON_LOCAL_RETURN$$$$$";

pub(super) fn is_return(insn: &Insn) -> bool {
    matches!(insn, Insn::Op(0xac..=0xb1))
}

/// The marker kotlinc places in front of a non-local return, named after what the return leaves.
/// A return that leaves the function it is finally written into has the empty name: every such
/// return krusty writes leaves that one function. A `break` or `continue` of a loop outside the
/// lambda names the loop (kotlinc's `nonLocalReturnLabel`).
pub(crate) fn non_local_return_marker(name: String) -> Node {
    Node::Insn(Insn::Method {
        op: INVOKESTATIC,
        owner: NON_LOCAL_RETURN.to_string(),
        name,
        desc: "()V".to_string(),
        interface: false,
    })
}

/// Whether `node` is the marker of a non-local return.
pub(crate) fn is_non_local_return_marker(node: &Node) -> bool {
    non_local_return_name(node).is_some()
}

/// The name of `node`, if it is the marker of a non-local return.
pub(crate) fn non_local_return_name(node: &Node) -> Option<&str> {
    match node {
        Node::Insn(Insn::Method {
            op: INVOKESTATIC,
            owner,
            name,
            ..
        }) if owner == NON_LOCAL_RETURN => Some(name),
        _ => None,
    }
}

/// kotlinc's `isMarkedReturn`: whether the node at `at` is a return right behind the marker.
pub(super) fn is_marked_return(nodes: &[Node], at: usize) -> bool {
    matches!(&nodes[at], Node::Insn(insn) if is_return(insn))
        && at
            .checked_sub(1)
            .is_some_and(|before| is_non_local_return_marker(&nodes[before]))
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
        if !is_return(insn) || is_marked_return(&node.nodes, at) {
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

/// Drop the `void` return that only keeps a `Unit` frame's after-return join inside the method.
///
/// The join's label stands between the lambda's own `void` return and this one, with nothing else
/// between them. Removing it puts the join on the inliner's end label, which is the instruction
/// after the own return becomes the closing-brace `nop`.
fn drop_unit_return_landing(node: &mut MethodNode) {
    let Some(landing) = node
        .nodes
        .iter()
        .rposition(|entry| matches!(entry, Node::Insn(_)))
    else {
        return;
    };
    if !matches!(node.nodes[landing], Node::Insn(Insn::Op(RETURN)))
        || is_marked_return(&node.nodes, landing)
    {
        return;
    }
    let Some(own) = node.nodes[..landing]
        .iter()
        .rposition(|entry| matches!(entry, Node::Insn(_)))
    else {
        return;
    };
    if !matches!(node.nodes[own], Node::Insn(Insn::Op(RETURN)))
        || is_marked_return(&node.nodes, own)
    {
        return;
    }
    let between = &node.nodes[own + 1..landing];
    if between.is_empty()
        || !between
            .iter()
            .all(|entry| matches!(entry, Node::Label(_) | Node::Line { .. }))
        || !between.iter().any(|entry| matches!(entry, Node::Label(_)))
    {
        return;
    }
    node.nodes.remove(landing);
}

/// Replace every return with `nop; goto end` and place a fresh label after the jump. A marked
/// return stays, with the fresh label after it.
pub(super) fn process_returns(node: &mut MethodNode, end: LabelId) {
    // A `Unit` lambda's early break targets a label between its own return and a second `void`
    // return that exists only so the method does not fall off that label. Dropping the second
    // return leaves the label on `end`, the instruction after the closing-brace `nop`.
    drop_unit_return_landing(node);
    let mut at = 0;
    while at < node.nodes.len() {
        if is_marked_return(&node.nodes, at) {
            let after = node.new_label();
            node.nodes.insert(at + 1, Node::Label(after));
            at += 2;
        } else if matches!(&node.nodes[at], Node::Insn(insn) if is_return(insn)) {
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
