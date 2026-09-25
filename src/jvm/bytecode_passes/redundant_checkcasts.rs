//! kotlinc's `RedundantCheckCastEliminationMethodTransformer`, over a [`MethodNode`]: a
//! `checkcast` goes when the value it casts already has exactly the requested class, or is `null`.
//!
//! Exact verifier identity is sufficient: no hierarchy query or source-local spelling is needed,
//! and a broader declared local type must not hide a narrower fact established on every incoming
//! edge. Like kotlinc, the pass keeps every cast of a method with a reified-operation marker, whose
//! casts the inliner still rewrites, and every cast to a multi-dimensional array class.
//!
//! What the verifier holds comes from the class-file analysis of the method as emitted
//! ([`StackTops`]), by instruction number; the pass therefore selects its casts on the method
//! before any other rewrite has moved an instruction.

use crate::jvm::method_node::{Insn, MethodNode, Node};

const INVOKESTATIC: u8 = 0xb8;
const CHECKCAST: u8 = 0xc0;

/// What the verifier holds on top of the operand stack before each instruction of a method.
pub(crate) trait StackTops {
    /// Whether the value on top of the stack before instruction `index` is `null` or of exactly
    /// `class` (an internal name, or an array descriptor).
    fn is_exactly(&self, index: usize, class: &str) -> bool;
}

fn is_reified_marker(insn: &Insn) -> bool {
    matches!(
        insn,
        Insn::Method { op: INVOKESTATIC, owner, name, .. }
            if owner == "kotlin/jvm/internal/Intrinsics" && name == "reifiedOperationMarker"
    )
}

/// The node positions of the casts in `method` that can go without changing verifier or runtime
/// behavior. `tops` is asked only when there is a cast to look at, and a method it has no answer
/// for keeps every cast.
pub(crate) fn select<'a, T: StackTops + 'a>(
    method: &MethodNode,
    tops: impl FnOnce() -> Option<&'a T>,
) -> Vec<usize> {
    let mut instructions = method
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(at, node)| match node {
            Node::Insn(insn) => Some((at, insn)),
            _ => None,
        });
    if instructions
        .clone()
        .any(|(_, insn)| is_reified_marker(insn))
        || !instructions
            .clone()
            .any(|(_, insn)| matches!(insn, Insn::Type { op: CHECKCAST, .. }))
    {
        return Vec::new();
    }
    let Some(tops) = tops() else {
        return Vec::new();
    };
    let mut selected = Vec::new();
    for (index, (at, insn)) in instructions.by_ref().enumerate() {
        let Insn::Type {
            op: CHECKCAST,
            class,
        } = insn
        else {
            continue;
        };
        if !class.starts_with("[[") && tops.is_exactly(index, class) {
            selected.push(at);
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALOAD: u8 = 0x19;
    const ARETURN: u8 = 0xb0;

    /// The stack top before each instruction, `None` where it is not a reference.
    struct Tops(Vec<Option<&'static str>>);

    impl StackTops for Tops {
        fn is_exactly(&self, index: usize, class: &str) -> bool {
            self.0[index].is_some_and(|top| top == "null" || top == class)
        }
    }

    fn cast(class: &str) -> Node {
        Node::Insn(Insn::Type {
            op: CHECKCAST,
            class: class.to_string(),
        })
    }

    fn method(nodes: Vec<Node>) -> MethodNode {
        let mut method = MethodNode::new(0x0009, "f", "(Ljava/lang/String;)Ljava/lang/Object;");
        method.nodes = nodes;
        method
    }

    fn aload_0() -> Node {
        Node::Insn(Insn::Var { op: ALOAD, slot: 0 })
    }

    #[test]
    fn a_cast_to_the_class_already_on_the_stack_or_of_null_goes() {
        let label = MethodNode::new(0, "", "").new_label();
        let method = method(vec![
            aload_0(),
            Node::Label(label),
            cast("java/lang/String"),
            cast("java/lang/Object"),
            cast("java/lang/String"),
            Node::Insn(Insn::Op(ARETURN)),
        ]);
        let tops = Tops(vec![
            None,
            Some("java/lang/String"),
            Some("java/lang/String"),
            Some("null"),
            Some("java/lang/String"),
        ]);
        assert_eq!(select(&method, || Some(&tops)), vec![2, 4]);
    }

    #[test]
    fn a_multi_dimensional_array_cast_stays() {
        let method = method(vec![aload_0(), cast("[[I"), Node::Insn(Insn::Op(ARETURN))]);
        let tops = Tops(vec![None, Some("[[I"), None]);
        assert_eq!(select(&method, || Some(&tops)), Vec::<usize>::new());
    }

    #[test]
    fn a_method_with_a_reified_marker_keeps_its_casts_without_asking() {
        let method = method(vec![
            Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: "kotlin/jvm/internal/Intrinsics".to_string(),
                name: "reifiedOperationMarker".to_string(),
                desc: "(ILjava/lang/String;)V".to_string(),
                interface: false,
            }),
            aload_0(),
            cast("java/lang/String"),
            Node::Insn(Insn::Op(ARETURN)),
        ]);
        let asked = std::cell::Cell::new(false);
        let tops = Tops(vec![None, None, Some("java/lang/String"), None]);
        let selected = select(&method, || {
            asked.set(true);
            Some(&tops)
        });
        assert_eq!(selected, Vec::<usize>::new());
        assert!(!asked.get());
    }

    #[test]
    fn a_method_without_facts_keeps_its_casts() {
        let method = method(vec![aload_0(), cast("java/lang/String")]);
        assert_eq!(select::<Tops>(&method, || None), Vec::<usize>::new());
    }
}
