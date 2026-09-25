//! The `kotlin/jvm/internal/InlineMarker` calls codegen leaves in a body for the passes that follow
//! (kotlinc's `inlineCodegenUtils.kt`): `mark(I)V` with an id pushed just before it delimits suspension
//! points and their parts, and `beforeInlineCall`/`afterInlineCall` bracket a call whose operand
//! stack FixStack must save.

use super::super::insn_list::{InsnList, NodeId};
use super::super::opcodes::*;
use crate::jvm::method_node::{Constant, Insn, Node};

pub(crate) const INLINE_MARKER_CLASS: &str = "kotlin/jvm/internal/InlineMarker";

/// A `mark(I)V` id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SuspendMarker {
    BeforeSuspend = 0,
    AfterSuspend = 1,
    ReturnsUnit = 2,
    FakeContinuation = 3,
    BeforeFakeContinuationConstructorCall = 4,
    AfterFakeContinuationConstructorCall = 5,
    BeforeUnboxInlineClass = 8,
    AfterUnboxInlineClass = 9,
    SuspendLambdaParameter = 10,
    BeforeSuspendUnitCall = 11,
    BeforeSuspendGenericCall = 12,
}

/// The two instructions of a `mark(id)` call, id first.
#[cfg(test)]
pub(crate) fn mark(marker: SuspendMarker) -> Vec<Node> {
    vec![
        Node::Insn(int_constant_insn(marker as i32)),
        Node::Insn(Insn::Method {
            op: INVOKESTATIC,
            owner: INLINE_MARKER_CLASS.to_string(),
            name: "mark".to_string(),
            desc: "(I)V".to_string(),
            interface: false,
        }),
    ]
}

/// A call of `InlineMarker.beforeInlineCall` or `afterInlineCall`.
#[cfg(test)]
pub(crate) fn inline_call_marker(before: bool) -> Node {
    Node::Insn(Insn::Method {
        op: INVOKESTATIC,
        owner: INLINE_MARKER_CLASS.to_string(),
        name: if before {
            "beforeInlineCall"
        } else {
            "afterInlineCall"
        }
        .to_string(),
        desc: "()V".to_string(),
        interface: false,
    })
}

/// The shortest instruction pushing `value`, as ASM's `InstructionAdapter.iconst` picks it.
pub(crate) fn int_constant_insn(value: i32) -> Insn {
    match value {
        -1..=5 => Insn::Op((i32::from(ICONST_0) + value) as u8),
        -128..=127 => Insn::Int {
            op: BIPUSH,
            operand: value,
        },
        -32768..=32767 => Insn::Int {
            op: SIPUSH,
            operand: value,
        },
        _ => Insn::Ldc(Constant::Int(value)),
    }
}

/// ASM's `intConstant` extension: the int an instruction pushes, if it is a constant push.
pub(crate) fn int_constant(node: &Node) -> Option<i32> {
    match node {
        Node::Insn(Insn::Op(op)) if (ICONST_M1..=ICONST_5).contains(op) => {
            Some(i32::from(*op) - i32::from(ICONST_0))
        }
        Node::Insn(Insn::Int {
            op: BIPUSH | SIPUSH,
            operand,
        }) => Some(*operand),
        Node::Insn(Insn::Ldc(Constant::Int(value))) => Some(*value),
        _ => None,
    }
}

/// `isInlineMarker(insn, name)`: a static call into `InlineMarker`, of `name` or, with `None`, of
/// `beforeInlineCall`/`afterInlineCall`.
pub(crate) fn is_inline_marker(node: &Node, name: Option<&str>) -> bool {
    match node {
        Node::Insn(Insn::Method {
            op: INVOKESTATIC,
            owner,
            name: called,
            desc,
            interface: false,
        }) if owner == INLINE_MARKER_CLASS => {
            let expected = match called.as_str() {
                "mark" => "(I)V",
                "beforeInlineCall" | "afterInlineCall" => "()V",
                _ => return false,
            };
            desc == expected
                && match name {
                    Some(name) => called == name,
                    None => called == "beforeInlineCall" || called == "afterInlineCall",
                }
        }
        _ => false,
    }
}

pub(crate) fn is_before_inline_marker(node: &Node) -> bool {
    is_inline_marker(node, Some("beforeInlineCall"))
}

pub(crate) fn is_after_inline_marker(node: &Node) -> bool {
    is_inline_marker(node, Some("afterInlineCall"))
}

/// `isSuspendInlineMarker`: any `mark(I)V` call.
pub(crate) fn is_suspend_inline_marker(node: &Node) -> bool {
    is_inline_marker(node, Some("mark"))
}

/// `isSuspendMarker(insn, id)`: a `mark` call whose previous node pushes `marker`'s id.
pub(crate) fn is_suspend_marker(insns: &InsnList, id: NodeId, marker: SuspendMarker) -> bool {
    is_suspend_inline_marker(insns.node(id))
        && insns
            .prev(id)
            .is_some_and(|prev| int_constant(insns.node(prev)) == Some(marker as i32))
}

/// `isFakeContinuationMarker`: the `aconst_null` that follows a fake-continuation mark.
pub(crate) fn is_fake_continuation_marker(insns: &InsnList, id: NodeId) -> bool {
    matches!(insns.node(id), Node::Insn(Insn::Op(ACONST_NULL)))
        && insns
            .prev(id)
            .is_some_and(|prev| is_suspend_marker(insns, prev, SuspendMarker::FakeContinuation))
}

/// `JvmAbi.isFakeLocalVariableForInline`: the `$i$f$`/`$i$a$` markers the inliner declares.
pub(crate) fn is_fake_local_variable_for_inline(name: &str) -> bool {
    name.starts_with("$i$f$") || name.starts_with("$i$a$")
}

#[cfg(test)]
mod tests {
    use super::{is_suspend_inline_marker, mark, SuspendMarker};
    use crate::jvm::method_node::{Insn, Node};

    #[test]
    fn suspend_marker_requires_the_exact_invocation_shape() {
        let exact = mark(SuspendMarker::BeforeSuspend)
            .pop()
            .expect("a marker call");
        assert!(is_suspend_inline_marker(&exact));

        let Node::Insn(Insn::Method {
            op,
            owner,
            name,
            desc,
            interface,
        }) = exact
        else {
            panic!("a marker is a method call");
        };
        let call = |desc: &str, interface| {
            Node::Insn(Insn::Method {
                op,
                owner: owner.clone(),
                name: name.clone(),
                desc: desc.to_string(),
                interface,
            })
        };
        assert!(!is_suspend_inline_marker(&call("()V", interface)));
        assert!(!is_suspend_inline_marker(&call(&desc, true)));
    }
}
