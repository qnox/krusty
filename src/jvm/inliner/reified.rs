//! Reified marker specialization over the symbolic method tree.

use crate::jvm::method_node::{Constant, Insn, MethodNode, Node};
use crate::jvm::reified_arguments::{ReifiedArgument, ReifiedArguments};
use crate::jvm::type_of::{TypeOfInsn, TYPE_OF_MARKER};

use super::InlineError;

const INVOKESTATIC: u8 = 0xb8;
const INTRINSICS: &str = "kotlin/jvm/internal/Intrinsics";
const MARKER_DESCRIPTOR: &str = "(ILjava/lang/String;)V";

enum Repoint {
    Class {
        instruction: usize,
        class: String,
        nullable: bool,
    },
    Forwarded {
        name_instruction: usize,
        name: String,
    },
    TypeOf {
        placeholder: usize,
        argument: String,
    },
}

struct Marker {
    operation: usize,
    name: usize,
    call: usize,
    repoint: Repoint,
}

/// Specialize every exact `reifiedOperationMarker` before ordinary inliner transforms inspect the
/// stack. Planning is all-or-nothing: malformed or unbound markers leave `node` untouched.
pub(super) fn specialize(
    node: &mut MethodNode,
    arguments: &ReifiedArguments,
) -> Result<(), InlineError> {
    let mut markers = Vec::new();
    for (call, entry) in node.nodes.iter().enumerate() {
        let Node::Insn(Insn::Method {
            op,
            owner,
            name,
            desc,
            interface,
        }) = entry
        else {
            continue;
        };
        if owner != INTRINSICS || name != "reifiedOperationMarker" {
            continue;
        }
        if *op != INVOKESTATIC || desc != MARKER_DESCRIPTOR || *interface {
            return Err(InlineError::MalformedReifiedMarker);
        }
        let operation = call
            .checked_sub(2)
            .ok_or(InlineError::MalformedReifiedMarker)?;
        let name_instruction = call - 1;
        let mode =
            pushed_int(node.nodes.get(operation)).ok_or(InlineError::MalformedReifiedMarker)?;
        let argument = loaded_string(node.nodes.get(name_instruction))
            .ok_or(InlineError::MalformedReifiedMarker)?;
        let repoint = if mode == TYPE_OF_MARKER {
            let placeholder = call + 1;
            if !matches!(
                node.nodes.get(placeholder),
                Some(Node::Insn(Insn::Op(0x01)))
            ) {
                return Err(InlineError::MalformedReifiedMarker);
            }
            if !arguments.type_of.contains_key(argument) {
                return Err(InlineError::MissingReifiedArgument(argument.to_owned()));
            }
            Repoint::TypeOf {
                placeholder,
                argument: argument.to_owned(),
            }
        } else {
            let target = (call + 1..node.nodes.len())
                .find(|&at| is_type_bearing(node.nodes.get(at)))
                .ok_or(InlineError::MalformedReifiedMarker)?;
            match arguments.classes.get(argument.trim_end_matches('?')) {
                Some(ReifiedArgument::Class { internal, nullable }) => Repoint::Class {
                    instruction: target,
                    class: internal.clone(),
                    nullable: *nullable || argument.ends_with('?'),
                },
                Some(ReifiedArgument::Forwarded { name, nullable }) => Repoint::Forwarded {
                    name_instruction,
                    name: format!(
                        "{name}{}",
                        if *nullable || argument.ends_with('?') {
                            "?"
                        } else {
                            ""
                        }
                    ),
                },
                None => return Err(InlineError::MissingReifiedArgument(argument.to_owned())),
            }
        };
        markers.push(Marker {
            operation,
            name: name_instruction,
            call,
            repoint,
        });
    }

    // Replacements that change the node count are applied last to first, so every recorded index
    // still names its node when its turn comes.
    let mut replacements: Vec<(usize, Vec<Node>)> = Vec::new();
    for marker in &markers {
        match &marker.repoint {
            Repoint::Class {
                instruction,
                class,
                nullable,
            } => {
                set_type_operand(&mut node.nodes[*instruction], class)?;
                erase_marker(node, marker);
                if *nullable && is_instance_of(&node.nodes[*instruction]) {
                    let check = nullable_instance_check(node, class);
                    replacements.push((*instruction, check));
                }
            }
            Repoint::Forwarded {
                name_instruction,
                name,
            } => {
                node.nodes[*name_instruction] =
                    Node::Insn(Insn::Ldc(Constant::String(name.clone().into())));
            }
            Repoint::TypeOf {
                placeholder,
                argument,
            } => {
                erase_marker(node, marker);
                let realization = arguments
                    .type_of
                    .get(argument)
                    .ok_or_else(|| InlineError::MissingReifiedArgument(argument.clone()))?;
                replacements.push((
                    *placeholder,
                    realization.iter().flat_map(type_of_nodes).collect(),
                ));
            }
        }
    }
    replacements.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (at, nodes) in replacements {
        node.nodes.splice(at..=at, nodes);
    }
    Ok(())
}

fn erase_marker(node: &mut MethodNode, marker: &Marker) {
    for at in [marker.operation, marker.name, marker.call] {
        node.nodes[at] = Node::Insn(Insn::Op(0x00));
    }
}

fn is_instance_of(node: &Node) -> bool {
    matches!(node, Node::Insn(Insn::Type { op: 0xc1, .. }))
}

/// kotlinc's `generateIsCheck` for a nullable type: `null` is an instance, so it is accepted
/// before the `instanceof` sees it.
fn nullable_instance_check(node: &mut MethodNode, class: &str) -> Vec<Node> {
    let null = node.new_label();
    let end = node.new_label();
    vec![
        Node::Insn(Insn::Op(0x59)),
        Node::Insn(Insn::Jump {
            op: 0xc6,
            target: null,
        }),
        Node::Insn(Insn::Type {
            op: 0xc1,
            class: class.to_owned(),
        }),
        Node::Insn(Insn::Jump {
            op: 0xa7,
            target: end,
        }),
        Node::Label(null),
        Node::Insn(Insn::Op(0x57)),
        Node::Insn(Insn::Op(0x04)),
        Node::Label(end),
    ]
}

fn pushed_int(node: Option<&Node>) -> Option<i32> {
    match node? {
        Node::Insn(Insn::Op(op @ 0x02..=0x08)) => Some(i32::from(*op) - 0x03),
        Node::Insn(Insn::Int {
            op: 0x10 | 0x11,
            operand,
        }) => Some(*operand),
        _ => None,
    }
}

fn loaded_string(node: Option<&Node>) -> Option<&str> {
    match node? {
        Node::Insn(Insn::Ldc(Constant::String(value))) => value.as_str(),
        _ => None,
    }
}

fn is_type_bearing(node: Option<&Node>) -> bool {
    matches!(
        node,
        Some(Node::Insn(
            Insn::Type {
                op: 0xbd | 0xc0 | 0xc1,
                ..
            } | Insn::MultiANewArray { .. }
                | Insn::Ldc(Constant::Class(_))
        ))
    )
}

fn set_type_operand(node: &mut Node, class: &str) -> Result<(), InlineError> {
    match node {
        Node::Insn(Insn::Type { class: target, .. })
        | Node::Insn(Insn::MultiANewArray { desc: target, .. })
        | Node::Insn(Insn::Ldc(Constant::Class(target))) => {
            *target = class.to_owned();
            Ok(())
        }
        _ => Err(InlineError::MalformedReifiedMarker),
    }
}

fn type_of_nodes(instruction: &TypeOfInsn) -> Vec<Node> {
    let instruction = match instruction {
        TypeOfInsn::LdcClass(class) => Insn::Ldc(Constant::Class(class.clone())),
        TypeOfInsn::PrimitiveClass(wrapper) => Insn::Field {
            op: 0xb2,
            owner: (*wrapper).to_owned(),
            name: "TYPE".to_owned(),
            desc: "Ljava/lang/Class;".to_owned(),
        },
        TypeOfInsn::LdcString(value) => Insn::Ldc(Constant::String(value.clone().into())),
        TypeOfInsn::PushInt(value) => push_int(*value),
        TypeOfInsn::AconstNull => Insn::Op(0x01),
        TypeOfInsn::Dup => Insn::Op(0x59),
        TypeOfInsn::New(class) => Insn::Type {
            op: 0xbb,
            class: (*class).to_owned(),
        },
        TypeOfInsn::ANewArray(class) => Insn::Type {
            op: 0xbd,
            class: (*class).to_owned(),
        },
        TypeOfInsn::AAStore => Insn::Op(0x53),
        TypeOfInsn::GetStatic {
            owner,
            name,
            descriptor,
        } => Insn::Field {
            op: 0xb2,
            owner: (*owner).to_owned(),
            name: (*name).to_owned(),
            desc: (*descriptor).to_owned(),
        },
        TypeOfInsn::InvokeStatic {
            owner,
            name,
            descriptor,
        } => method(0xb8, owner, name, descriptor),
        TypeOfInsn::InvokeVirtual {
            owner,
            name,
            descriptor,
        } => method(0xb6, owner, name, descriptor),
        TypeOfInsn::InvokeSpecial {
            owner,
            name,
            descriptor,
        } => method(0xb7, owner, name, descriptor),
        TypeOfInsn::ReifiedMarker(argument) => {
            return vec![
                Node::Insn(push_int(TYPE_OF_MARKER)),
                Node::Insn(Insn::Ldc(Constant::String(argument.clone().into()))),
                Node::Insn(method(
                    INVOKESTATIC,
                    INTRINSICS,
                    "reifiedOperationMarker",
                    MARKER_DESCRIPTOR,
                )),
            ];
        }
    };
    vec![Node::Insn(instruction)]
}

fn push_int(value: i32) -> Insn {
    match value {
        -1..=5 => Insn::Op((0x03 + value) as u8),
        -128..=127 => Insn::Int {
            op: 0x10,
            operand: value,
        },
        _ => Insn::Int {
            op: 0x11,
            operand: value,
        },
    }
}

fn method(op: u8, owner: &str, name: &str, desc: &str) -> Insn {
    Insn::Method {
        op,
        owner: owner.to_owned(),
        name: name.to_owned(),
        desc: desc.to_owned(),
        interface: false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn marker(argument: &str, mode: i32) -> Vec<Node> {
        vec![
            Node::Insn(push_int(mode)),
            Node::Insn(Insn::Ldc(Constant::String(argument.into()))),
            Node::Insn(method(
                INVOKESTATIC,
                INTRINSICS,
                "reifiedOperationMarker",
                MARKER_DESCRIPTOR,
            )),
        ]
    }

    #[test]
    fn concrete_argument_erases_marker_and_repoints_symbolic_type() {
        let mut node = MethodNode::new(0x0008, "isT", "(Ljava/lang/Object;)Z");
        node.nodes = marker("T", 3);
        node.nodes.push(Node::Insn(Insn::Type {
            op: 0xc1,
            class: "java/lang/Object".into(),
        }));
        let arguments = ReifiedArguments {
            classes: HashMap::from([(
                "T".to_owned(),
                ReifiedArgument::Class {
                    internal: "java/lang/String".to_owned(),
                    nullable: false,
                },
            )]),
            ..Default::default()
        };

        specialize(&mut node, &arguments).expect("specializes");

        assert!(node.nodes[..3]
            .iter()
            .all(|node| matches!(node, Node::Insn(Insn::Op(0x00)))));
        assert!(matches!(
            &node.nodes[3],
            Node::Insn(Insn::Type { op: 0xc1, class }) if class == "java/lang/String"
        ));
    }

    #[test]
    fn a_nullable_instance_check_accepts_null_before_the_instanceof() {
        for (marker_name, nullable) in [("T?", false), ("T", true)] {
            let mut node = MethodNode::new(0x0008, "isT", "(Ljava/lang/Object;)Z");
            node.nodes = marker(marker_name, 3);
            node.nodes.push(Node::Insn(Insn::Type {
                op: 0xc1,
                class: "java/lang/Object".into(),
            }));
            let arguments = ReifiedArguments {
                classes: HashMap::from([(
                    "T".to_owned(),
                    ReifiedArgument::Class {
                        internal: "java/lang/String".to_owned(),
                        nullable,
                    },
                )]),
                ..Default::default()
            };

            specialize(&mut node, &arguments).expect("specializes");

            // The first two labels a fresh method mints are the ones the check introduced.
            let mut fresh = MethodNode::new(0x0008, "isT", "(Ljava/lang/Object;)Z");
            let null = fresh.new_label();
            let end = fresh.new_label();
            let nop = Node::Insn(Insn::Op(0x00));
            assert_eq!(
                node.nodes,
                vec![
                    nop.clone(),
                    nop.clone(),
                    nop,
                    Node::Insn(Insn::Op(0x59)),
                    Node::Insn(Insn::Jump {
                        op: 0xc6,
                        target: null,
                    }),
                    Node::Insn(Insn::Type {
                        op: 0xc1,
                        class: "java/lang/String".into(),
                    }),
                    Node::Insn(Insn::Jump {
                        op: 0xa7,
                        target: end,
                    }),
                    Node::Label(null),
                    Node::Insn(Insn::Op(0x57)),
                    Node::Insn(Insn::Op(0x04)),
                    Node::Label(end),
                ],
                "{marker_name} with a nullable={nullable} argument"
            );
        }
    }

    #[test]
    fn forwarded_argument_keeps_marker_and_renames_nullability() {
        let mut node = MethodNode::new(0x0008, "isT", "(Ljava/lang/Object;)Z");
        node.nodes = marker("T?", 3);
        node.nodes.push(Node::Insn(Insn::Type {
            op: 0xc1,
            class: "java/lang/Object".into(),
        }));
        let arguments = ReifiedArguments {
            classes: HashMap::from([(
                "T".to_owned(),
                ReifiedArgument::Forwarded {
                    name: "R".to_owned(),
                    nullable: false,
                },
            )]),
            ..Default::default()
        };

        specialize(&mut node, &arguments).expect("forwards");

        assert!(matches!(
            &node.nodes[1],
            Node::Insn(Insn::Ldc(Constant::String(name))) if name.as_str() == Some("R?")
        ));
        assert!(matches!(
            &node.nodes[2],
            Node::Insn(Insn::Method { name, .. }) if name == "reifiedOperationMarker"
        ));
    }

    #[test]
    fn type_of_placeholder_is_replaced_by_its_symbolic_realization() {
        let mut node = MethodNode::new(0x0008, "type", "()Ljava/lang/Object;");
        node.nodes = marker("T", TYPE_OF_MARKER);
        node.nodes.push(Node::Insn(Insn::Op(0x01)));
        let arguments = ReifiedArguments {
            type_of: HashMap::from([(
                "T".to_owned(),
                vec![TypeOfInsn::LdcClass("java/lang/String".to_owned())],
            )]),
            ..Default::default()
        };

        specialize(&mut node, &arguments).expect("specializes typeOf");

        assert!(matches!(
            &node.nodes[3],
            Node::Insn(Insn::Ldc(Constant::Class(class))) if class == "java/lang/String"
        ));
    }

    #[test]
    fn an_unbound_marker_rejects_without_mutating_the_method() {
        let mut node = MethodNode::new(0x0008, "isT", "(Ljava/lang/Object;)Z");
        node.nodes = marker("T", 3);
        node.nodes.push(Node::Insn(Insn::Type {
            op: 0xc1,
            class: "java/lang/Object".into(),
        }));
        let original = node.clone();

        assert_eq!(
            specialize(&mut node, &ReifiedArguments::default()),
            Err(InlineError::MissingReifiedArgument("T".to_owned())),
        );
        assert_eq!(node, original);
    }

    #[test]
    fn a_similarly_named_non_marker_method_is_rejected() {
        let mut node = MethodNode::new(0x0008, "isT", "()V");
        node.nodes = marker("T", 3);
        let Node::Insn(Insn::Method {
            desc, interface, ..
        }) = &mut node.nodes[2]
        else {
            unreachable!()
        };
        *desc = "()V".to_owned();
        *interface = true;
        let original = node.clone();

        assert_eq!(
            specialize(&mut node, &ReifiedArguments::default()),
            Err(InlineError::MalformedReifiedMarker),
        );
        assert_eq!(node, original);
    }
}
