//! kotlinc's `ChangeBoxingMethodTransformer`: in a suspend function, box primitives through
//! `kotlin.coroutines.jvm.internal.Boxing` instead of the wrappers' `valueOf`.

use super::super::insn_list::EditableMethod;
use super::super::opcodes::INVOKESTATIC;
use crate::jvm::method_node::{Insn, Node};

const BOXING_CLASS: &str = "kotlin/coroutines/jvm/internal/Boxing";

/// The JVM primitives' wrapper classes and descriptors, with the `Boxing` method for each
/// (`JvmPrimitiveType`).
const WRAPPERS: [(&str, &str, &str); 8] = [
    ("java/lang/Boolean", "Z", "boxBoolean"),
    ("java/lang/Character", "C", "boxChar"),
    ("java/lang/Byte", "B", "boxByte"),
    ("java/lang/Short", "S", "boxShort"),
    ("java/lang/Integer", "I", "boxInt"),
    ("java/lang/Float", "F", "boxFloat"),
    ("java/lang/Long", "J", "boxLong"),
    ("java/lang/Double", "D", "boxDouble"),
];

/// The `Boxing` method replacing `node` when it is a primitive's `valueOf` boxing call.
fn replacement(node: &Node) -> Option<&'static str> {
    let Node::Insn(Insn::Method {
        op: INVOKESTATIC,
        owner,
        name,
        desc,
        ..
    }) = node
    else {
        return None;
    };
    if name != "valueOf" {
        return None;
    }
    WRAPPERS.iter().find_map(|(wrapper, primitive, boxing)| {
        (owner == wrapper && *desc == format!("({primitive})L{wrapper};")).then_some(*boxing)
    })
}

pub(crate) fn change_boxing(method: &mut EditableMethod) {
    for id in method.insns.ids() {
        let Some(boxing) = replacement(method.insns.node(id)) else {
            continue;
        };
        let Node::Insn(Insn::Method { desc, .. }) = method.insns.node(id).clone() else {
            unreachable!("a boxing call is a method instruction");
        };
        method.insns.set(
            id,
            Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: BOXING_CLASS.to_string(),
                name: boxing.to_string(),
                desc,
                interface: false,
            }),
        );
    }
}
