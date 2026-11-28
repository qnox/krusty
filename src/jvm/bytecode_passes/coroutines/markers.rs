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
pub enum SuspendMarker {
    BeforeSuspend = 0,
    AfterSuspend = 1,
    ReturnsUnit = 2,
    FakeContinuation = 3,
    BeforeFakeContinuationConstructorCall = 4,
    AfterFakeContinuationConstructorCall = 5,
    BeforeInlineSuspend = 6,
    AfterInlineSuspend = 7,
    BeforeUnboxInlineClass = 8,
    AfterUnboxInlineClass = 9,
    SuspendLambdaParameter = 10,
    BeforeSuspendUnitCall = 11,
    BeforeSuspendGenericCall = 12,
}

/// The two instructions of a `mark(id)` call, id first (`addSuspendMarker`).
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

/// A suspend inline function that returns a type parameter carries `mark(BeforeSuspendGenericCall)`
/// where a `Unit`-returning one carries `mark(BeforeSuspendUnitCall)`. At a call that binds the
/// type parameter to `Unit` the body is a unit suspend call, which lets a tail call return
/// without a state machine (`suspend fun f() = suspendCoroutine<Unit> { … }`).
///
/// Only the suspension point that is the body's own result is bound: the one whose closing marker
/// is followed directly by the return, or by the end of an inlined body whose returns
/// the inliner already turned into jumps. A generic point nested in the body, such as one in a
/// `Unit`-returning function, has a result of its own that is not the call's.
pub fn bind_generic_suspend_call_to_unit(nodes: &mut [Node]) {
    let is_after = |at: usize| {
        at > 0
            && is_suspend_inline_marker(&nodes[at])
            && int_constant(&nodes[at - 1]) == Some(SuspendMarker::AfterSuspend as i32)
    };
    let Some(close) = (1..nodes.len()).rev().find(|&at| is_after(at)) else {
        return;
    };
    let returns_at_once = nodes[close + 1..]
        .iter()
        .find(|node| {
            !matches!(node, Node::Label(_) | Node::Line { .. })
                && !matches!(node, Node::Insn(Insn::Op(NOP)))
                && !matches!(node, Node::Insn(Insn::Jump { op: GOTO, .. }))
        })
        .is_none_or(|node| matches!(node, Node::Insn(Insn::Op(ARETURN))));
    if !returns_at_once {
        return;
    }
    // The opening marker that pairs with `close`, skipping the pairs nested inside it.
    let marker_id = |at: usize| {
        if at > 0 && is_suspend_inline_marker(&nodes[at]) {
            int_constant(&nodes[at - 1])
        } else {
            None
        }
    };
    let mut nested = 0usize;
    let mut open = None;
    for at in (1..close).rev() {
        match marker_id(at) {
            Some(id) if id == SuspendMarker::AfterSuspend as i32 => nested += 1,
            Some(id) if id == SuspendMarker::BeforeSuspend as i32 => {
                if nested == 0 {
                    open = Some(at);
                    break;
                }
                nested -= 1;
            }
            _ => {}
        }
    }
    // Its kind marker follows it directly.
    let generic = open.map(|open| open + 2).filter(|&at| {
        at < nodes.len() && marker_id(at) == Some(SuspendMarker::BeforeSuspendGenericCall as i32)
    });
    if let Some(generic) = generic {
        nodes[generic - 1] = Node::Insn(int_constant_insn(
            SuspendMarker::BeforeSuspendUnitCall as i32,
        ));
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
pub fn is_inline_marker(node: &Node, name: Option<&str>) -> bool {
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
                "beforeInlineCall" | "beforeInlineCallWithReservedLocals" | "afterInlineCall" => {
                    "()V"
                }
                _ => return false,
            };
            desc == expected
                && match name {
                    Some(name) => called == name,
                    None => {
                        called == "beforeInlineCall"
                            || called == "beforeInlineCallWithReservedLocals"
                            || called == "afterInlineCall"
                    }
                }
        }
        _ => false,
    }
}

pub fn is_before_inline_marker(node: &Node) -> bool {
    is_inline_marker(node, Some("beforeInlineCall"))
        || is_inline_marker(node, Some("beforeInlineCallWithReservedLocals"))
}

/// An opening marker whose stack spill words were reserved before the next inline-frame local.
pub(crate) fn has_reserved_inline_locals(node: &Node) -> bool {
    is_inline_marker(node, Some("beforeInlineCallWithReservedLocals"))
}

pub fn is_after_inline_marker(node: &Node) -> bool {
    is_inline_marker(node, Some("afterInlineCall"))
}

/// A declaration-body marker bracketing one emitted copy of a `finally`. Unlike the inline-call
/// stack markers, this metadata is consumed while copying the body and never executes in the
/// finished caller.
pub(crate) fn is_finally_marker(node: &Node) -> bool {
    matches!(
        node,
        Node::Insn(Insn::Method {
            op: INVOKESTATIC,
            owner,
            name,
            desc,
            interface: false,
        }) if owner == INLINE_MARKER_CLASS
            && matches!(name.as_str(), "finallyStart" | "finallyEnd")
            && desc == "(I)V"
    )
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

/// `collectSuspendLambdaParameterSlots`: the slots whose store a `mark(10)` follows.
pub(crate) fn suspend_lambda_parameter_slots(insns: &InsnList) -> Vec<u16> {
    insns
        .ids()
        .into_iter()
        .filter(|&id| is_suspend_marker(insns, id, SuspendMarker::SuspendLambdaParameter))
        .filter_map(|id| {
            let store = insns.prev(id).and_then(|push| insns.prev(push))?;
            match insns.node(store) {
                Node::Insn(Insn::Var { slot, .. }) => Some(*slot),
                _ => None,
            }
        })
        .collect()
}

/// `JvmAbi.isFakeLocalVariableForInline`: the `$i$f$`/`$i$a$` markers the inliner declares.
pub(crate) fn is_fake_local_variable_for_inline(name: &str) -> bool {
    name.starts_with("$i$f$") || name.starts_with("$i$a$")
}

#[cfg(test)]
mod tests {
    use super::{
        bind_generic_suspend_call_to_unit, int_constant, is_suspend_inline_marker, mark,
        SuspendMarker,
    };
    use crate::jvm::method_node::{Insn, Node};

    #[test]
    fn only_the_generic_marker_of_the_closing_point_binds() {
        let mut nodes = mark(SuspendMarker::BeforeSuspend);
        nodes.extend(mark(SuspendMarker::BeforeSuspendGenericCall));
        nodes.extend(mark(SuspendMarker::BeforeSuspend));
        nodes.extend(mark(SuspendMarker::BeforeSuspendGenericCall));
        nodes.extend(mark(SuspendMarker::AfterSuspend));
        nodes.extend(mark(SuspendMarker::AfterSuspend));
        nodes.push(Node::Insn(Insn::Op(
            crate::jvm::bytecode_passes::opcodes::ARETURN,
        )));
        bind_generic_suspend_call_to_unit(&mut nodes);
        let ids = nodes.iter().filter_map(int_constant).collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                SuspendMarker::BeforeSuspend as i32,
                SuspendMarker::BeforeSuspendUnitCall as i32,
                SuspendMarker::BeforeSuspend as i32,
                SuspendMarker::BeforeSuspendGenericCall as i32,
                SuspendMarker::AfterSuspend as i32,
                SuspendMarker::AfterSuspend as i32,
            ]
        );
    }

    #[test]
    fn a_tail_point_after_an_earlier_generic_point_binds_only_the_tail() {
        let mut nodes = mark(SuspendMarker::BeforeSuspend);
        nodes.extend(mark(SuspendMarker::BeforeSuspendGenericCall));
        nodes.extend(mark(SuspendMarker::AfterSuspend));
        nodes.push(Node::Insn(Insn::Op(
            crate::jvm::bytecode_passes::opcodes::POP,
        )));
        nodes.extend(mark(SuspendMarker::BeforeSuspend));
        nodes.extend(mark(SuspendMarker::BeforeSuspendGenericCall));
        nodes.extend(mark(SuspendMarker::AfterSuspend));
        nodes.push(Node::Insn(Insn::Op(
            crate::jvm::bytecode_passes::opcodes::ARETURN,
        )));
        bind_generic_suspend_call_to_unit(&mut nodes);
        let ids = nodes.iter().filter_map(int_constant).collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                SuspendMarker::BeforeSuspend as i32,
                SuspendMarker::BeforeSuspendGenericCall as i32,
                SuspendMarker::AfterSuspend as i32,
                SuspendMarker::BeforeSuspend as i32,
                SuspendMarker::BeforeSuspendUnitCall as i32,
                SuspendMarker::AfterSuspend as i32,
            ]
        );
    }

    #[test]
    fn a_nested_generic_suspend_marker_is_left_alone() {
        let mut nodes = mark(SuspendMarker::BeforeSuspend);
        nodes.extend(mark(SuspendMarker::BeforeSuspendGenericCall));
        nodes.extend(mark(SuspendMarker::AfterSuspend));
        nodes.push(Node::Insn(Insn::Op(
            crate::jvm::bytecode_passes::opcodes::POP,
        )));
        nodes.push(Node::Insn(Insn::Op(
            crate::jvm::bytecode_passes::opcodes::ARETURN,
        )));
        let before = nodes.clone();
        bind_generic_suspend_call_to_unit(&mut nodes);
        assert_eq!(nodes, before);
    }

    #[test]
    fn a_generic_suspend_marker_binds_to_the_unit_call_marker() {
        let mut nodes = mark(SuspendMarker::BeforeSuspend);
        nodes.extend(mark(SuspendMarker::BeforeSuspendGenericCall));
        nodes.extend(mark(SuspendMarker::AfterSuspend));
        nodes.push(Node::Insn(Insn::Op(
            crate::jvm::bytecode_passes::opcodes::ARETURN,
        )));
        bind_generic_suspend_call_to_unit(&mut nodes);
        let ids = nodes.iter().filter_map(int_constant).collect::<Vec<_>>();
        assert_eq!(
            ids,
            [
                SuspendMarker::BeforeSuspend as i32,
                SuspendMarker::BeforeSuspendUnitCall as i32,
                SuspendMarker::AfterSuspend as i32,
            ]
        );
    }

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
