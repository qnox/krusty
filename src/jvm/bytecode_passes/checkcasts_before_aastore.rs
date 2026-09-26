//! kotlinc's `RedundantCheckcastsBeforeAastoreMethodTransformer`, over a [`MethodNode`]: a
//! `checkcast` whose next node is an `aastore` goes. The array store checks the element's class
//! itself, and the verifier accepts any reference as the stored value.
//!
//! When the node before such a cast is a reified-operation marker, the cast is the marker's
//! placeholder, and kotlinc removes the three nodes before the cast with it: the marker call and
//! its two arguments (`iconst <kind>; ldc "<parameter>"`). What the inliner would specialize is
//! then gone, so a call site of the function stores its argument uncast as well.
//!
//! kotlinc walks ASM's node list, which holds a label only when something refers to it. A label
//! nothing names is therefore not a node here: it separates neither the cast from its `aastore`
//! nor the marker from its cast, and it is not one of the three nodes removed with the marker.

use std::collections::BTreeSet;

use super::redundant_checkcasts::is_reified_marker;
use crate::jvm::method_node::{Insn, MethodNode, Node};

const CHECKCAST: u8 = 0xc0;
const AASTORE: u8 = 0x53;

/// How many nodes kotlinc removes before a cast that follows a reified-operation marker.
const MARKER_NODES: usize = 3;

/// Remove every `checkcast` right before an `aastore`, with the reified-operation marker right
/// before it; `true` when any went.
pub(crate) fn remove(method: &mut MethodNode) -> bool {
    let referenced = method.referenced_labels();
    // The positions of the nodes ASM's list would hold.
    let listed: Vec<usize> = method
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| !matches!(node, Node::Label(label) if !referenced.contains(label)))
        .map(|(at, _)| at)
        .collect();
    let insn = |at: usize| match &method.nodes[at] {
        Node::Insn(insn) => Some(insn),
        _ => None,
    };
    let mut removed = BTreeSet::new();
    for (k, &at) in listed.iter().enumerate() {
        if !matches!(insn(at), Some(Insn::Type { op: CHECKCAST, .. })) {
            continue;
        }
        let stored = listed
            .get(k + 1)
            .is_some_and(|&next| matches!(insn(next), Some(Insn::Op(AASTORE))));
        if !stored {
            continue;
        }
        // The nodes before the cast still in the list, nearest first.
        let before: Vec<usize> = listed[..k]
            .iter()
            .rev()
            .copied()
            .filter(|at| !removed.contains(at))
            .take(MARKER_NODES)
            .collect();
        let marked = before
            .first()
            .and_then(|&previous| insn(previous))
            .is_some_and(is_reified_marker);
        removed.insert(at);
        if marked {
            removed.extend(before);
        }
    }
    if removed.is_empty() {
        return false;
    }
    let mut position = 0;
    method.nodes.retain(|_| {
        position += 1;
        !removed.contains(&(position - 1))
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::method_node::{Constant, LabelId};

    const ALOAD: u8 = 0x19;
    const ICONST_0: u8 = 0x03;
    const ICONST_1: u8 = 0x04;
    const ANEWARRAY: u8 = 0xbd;
    const ARETURN: u8 = 0xb0;
    const GOTO: u8 = 0xa7;
    const INVOKESTATIC: u8 = 0xb8;

    fn op(op: u8) -> Node {
        Node::Insn(Insn::Op(op))
    }

    fn cast(class: &str) -> Node {
        Node::Insn(Insn::Type {
            op: CHECKCAST,
            class: class.to_string(),
        })
    }

    fn aload(slot: u16) -> Node {
        Node::Insn(Insn::Var { op: ALOAD, slot })
    }

    /// `kotlin.jvm.internal.Intrinsics.reifiedOperationMarker(1, "T")`, the placeholder of an
    /// `as T` on a reified `T`.
    fn marker() -> [Node; 3] {
        call_named_marker("(ILjava/lang/String;)V", false)
    }

    /// A static call spelled `Intrinsics.reifiedOperationMarker` with `desc` and `interface`, after
    /// the marker's two arguments.
    fn call_named_marker(desc: &str, interface: bool) -> [Node; 3] {
        [
            op(ICONST_1),
            Node::Insn(Insn::Ldc(Constant::String("T".into()))),
            Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: "kotlin/jvm/internal/Intrinsics".to_string(),
                name: "reifiedOperationMarker".to_string(),
                desc: desc.to_string(),
                interface,
            }),
        ]
    }

    /// `new Object[1]` with the store of `aload_0` and `element` into it, then its return.
    fn store(element: Vec<Node>) -> MethodNode {
        let mut method = MethodNode::new(0x0009, "f", "(Ljava/lang/Object;)[Ljava/lang/Object;");
        method.nodes = vec![
            op(ICONST_1),
            Node::Insn(Insn::Type {
                op: ANEWARRAY,
                class: "java/lang/Object".to_string(),
            }),
            Node::Insn(Insn::Op(0x59)),
            op(ICONST_0),
            aload(0),
        ];
        method.nodes.extend(element);
        method.nodes.extend([op(AASTORE), op(ARETURN)]);
        method
    }

    fn instructions(method: &MethodNode) -> Vec<Insn> {
        method.instructions().cloned().collect()
    }

    #[test]
    fn a_cast_right_before_an_array_store_goes() {
        let mut method = store(vec![cast("java/lang/String")]);
        assert!(remove(&mut method));
        assert_eq!(method, store(Vec::new()));
    }

    #[test]
    fn only_the_cast_next_to_the_store_goes() {
        // `checkcast String; checkcast CharSequence; aastore`: the first cast is followed by the
        // second, not by the store.
        let mut method = store(vec![
            cast("java/lang/String"),
            cast("java/lang/CharSequence"),
        ]);
        assert!(remove(&mut method));
        assert_eq!(method, store(vec![cast("java/lang/String")]));
    }

    #[test]
    fn a_cast_before_anything_else_stays() {
        let mut method = store(vec![cast("java/lang/String"), op(0x00)]);
        let emitted = method.clone();
        assert!(!remove(&mut method));
        assert_eq!(method, emitted);
    }

    #[test]
    fn a_reified_cast_goes_with_its_marker() {
        let mut element: Vec<Node> = marker().into();
        element.push(cast("java/lang/Object"));
        let mut method = store(element);
        assert!(remove(&mut method));
        assert_eq!(method, store(Vec::new()));
    }

    /// Only the cast goes after a call that has the marker's name but not its descriptor, or that
    /// is an interface method: the call and its arguments are ordinary bytecode.
    #[test]
    fn a_call_that_only_shares_the_markers_name_stays() {
        for (desc, interface) in [
            ("(ILjava/lang/String;)Ljava/lang/Object;", false),
            ("(ILjava/lang/String;)V", true),
        ] {
            let mut element: Vec<Node> = call_named_marker(desc, interface).into();
            element.push(cast("java/lang/Object"));
            let mut method = store(element);
            assert!(remove(&mut method), "{desc} interface={interface}");
            assert_eq!(
                method,
                store(call_named_marker(desc, interface).into()),
                "{desc} interface={interface}"
            );
        }
    }

    #[test]
    fn a_marker_separated_from_the_cast_stays() {
        // A line number is a node of its own, so the marker is not the cast's previous node.
        let mut stored = store(Vec::new());
        let line = stored.new_label();
        let at = stored.nodes.len() - 2;
        let mut element: Vec<Node> = marker().into();
        element.extend([
            Node::Label(line),
            Node::Line {
                line: 3,
                start: line,
            },
            cast("java/lang/Object"),
        ]);
        stored.nodes.splice(at..at, element);
        assert!(remove(&mut stored));
        let [kind, name, call] = marker();
        assert_eq!(
            instructions(&stored),
            instructions(&store(vec![kind, name, call]))
        );
    }

    #[test]
    fn an_unreferenced_label_separates_nothing() {
        let mut method = store(Vec::new());
        let loose = method.new_label();
        let at = method.nodes.len() - 2;
        let mut element: Vec<Node> = marker().into();
        element.insert(1, Node::Label(loose));
        element.extend([cast("java/lang/Object"), Node::Label(loose)]);
        method.nodes.splice(at..at, element);
        assert!(remove(&mut method));
        assert_eq!(instructions(&method), instructions(&store(Vec::new())));
    }

    #[test]
    fn a_label_a_jump_names_separates_the_cast_from_the_store() {
        let mut method = store(Vec::new());
        let target: LabelId = method.new_label();
        let at = method.nodes.len() - 2;
        method
            .nodes
            .splice(at..at, [cast("java/lang/String"), Node::Label(target)]);
        method
            .nodes
            .push(Node::Insn(Insn::Jump { op: GOTO, target }));
        let emitted = method.clone();
        assert!(!remove(&mut method));
        assert_eq!(method, emitted);
    }
}
