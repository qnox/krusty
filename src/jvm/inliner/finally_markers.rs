//! Consume declaration-body `InlineMarker.finallyStart`/`finallyEnd` metadata from an inline copy.
//!
//! The declaration method retains these calls for a future inliner, but kotlinc does not copy them
//! into the finished caller. Krusty's currently supported bodies already contain their duplicated
//! cleanup; this boundary removes each exact marker together with its constant depth operand.

use crate::jvm::bytecode_passes::coroutines::markers::{int_constant, is_finally_marker};
use crate::jvm::method_node::MethodNode;

use super::InlineError;

pub(super) fn remove(method: &mut MethodNode) -> Result<(), InlineError> {
    let mut remove = vec![false; method.nodes.len()];
    for (index, node) in method.nodes.iter().enumerate() {
        if !is_finally_marker(node) {
            continue;
        }
        let Some(argument) = index.checked_sub(1) else {
            return Err(InlineError::MalformedFinallyMarker);
        };
        if int_constant(&method.nodes[argument]).is_none() || remove[argument] {
            return Err(InlineError::MalformedFinallyMarker);
        }
        remove[argument] = true;
        remove[index] = true;
    }
    let mut index = 0usize;
    method.nodes.retain(|_| {
        let keep = !remove[index];
        index += 1;
        keep
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::bytecode_passes::coroutines::markers::{
        int_constant_insn, INLINE_MARKER_CLASS,
    };
    use crate::jvm::method_node::{Insn, Node};

    fn marker(name: &str) -> Node {
        Node::Insn(Insn::Method {
            op: 0xb8,
            owner: INLINE_MARKER_CLASS.to_string(),
            name: name.to_string(),
            desc: "(I)V".to_string(),
            interface: false,
        })
    }

    #[test]
    fn removes_each_marker_with_its_depth_operand() {
        let mut method = MethodNode::new(0x0008, "f", "()V");
        method.nodes = vec![
            Node::Insn(int_constant_insn(1)),
            marker("finallyStart"),
            Node::Insn(Insn::Op(0x00)),
            Node::Insn(int_constant_insn(1)),
            marker("finallyEnd"),
            Node::Insn(Insn::Op(0xb1)),
        ];
        remove(&mut method).expect("exact marker pairs");
        assert_eq!(method.nodes.len(), 2);
        assert!(matches!(method.nodes[0], Node::Insn(Insn::Op(0x00))));
        assert!(matches!(method.nodes[1], Node::Insn(Insn::Op(0xb1))));
    }

    #[test]
    fn rejects_a_marker_without_a_constant_depth() {
        let mut method = MethodNode::new(0x0008, "f", "()V");
        method.nodes = vec![Node::Insn(Insn::Op(0x00)), marker("finallyStart")];
        assert_eq!(
            remove(&mut method),
            Err(InlineError::MalformedFinallyMarker)
        );
    }
}
