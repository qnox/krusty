//! The regenerated class's constructor: kotlinc's `extractParametersMappingAndPatchConstructor` and
//! `generateConstructorAndFields` for an object none of whose constructor arguments is an inline
//! lambda.
//!
//! The original constructor stores each captured value (`$x`, `this$0`, `receiver$0`) into its
//! field straight from its parameter. Those stores are taken out of the copied body; the new
//! constructor declares the captured fields afresh and stores every one of them first, then runs
//! what is left of the original body.

use std::collections::HashMap;

use crate::jvm::method_node::{Insn, MethodNode, Node};

use super::{is_captured_field_name, RegenerationError};

const ALOAD: u8 = 0x19;
const PUTFIELD: u8 = 0xb5;
const GETFIELD: u8 = 0xb4;

/// A captured field the new constructor declares and stores from its parameter at `slot`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedField {
    pub name: String,
    pub desc: String,
    pub slot: u16,
}

/// The constructor of the regenerated class.
pub(super) struct Constructor {
    /// The new constructor's descriptor (`newConstructorDescriptor`).
    pub desc: String,
    /// The captured fields it stores, in parameter order.
    pub fields: Vec<CapturedField>,
    /// The original body without its captured-field stores, still to be copied.
    pub body: MethodNode,
}

/// A captured field the original class declares.
pub(super) struct DeclaredCapture<'a> {
    pub name: &'a str,
    pub desc: &'a str,
}

/// Take the captured-field stores out of `original`, the constructor of `owner`
/// (`findCapturedFieldAssignmentInstructions`), and plan the new constructor for a call through
/// `call_desc`. Every field of `declared` must be stored exactly once, as
/// `aload 0; <load of its parameter>; putfield owner.field`, from a parameter of its own type;
/// any other store or read of a captured field is refused, since the copy declares only these.
pub(super) fn extract(
    original: &MethodNode,
    owner: &str,
    declared: &[DeclaredCapture<'_>],
    call_desc: &str,
) -> Result<Constructor, RegenerationError> {
    let unsupported = RegenerationError::Unsupported;
    let arguments =
        argument_descriptors(call_desc).ok_or(unsupported("a malformed constructor descriptor"))?;
    // The parameter that starts at each slot.
    let mut parameters: HashMap<u16, &str> = HashMap::new();
    let mut slot = 1u16;
    for argument in &arguments {
        parameters.insert(slot, argument);
        slot += if matches!(argument.as_bytes()[0], b'J' | b'D') {
            2
        } else {
            1
        };
    }

    let mut body = original.clone();
    let mut captured: Vec<CapturedField> = Vec::new();
    let mut remove = Vec::new();
    for (at, entry) in body.nodes.iter().enumerate() {
        let Node::Insn(Insn::Field {
            op: PUTFIELD,
            owner: field_owner,
            name,
            desc,
        }) = entry
        else {
            continue;
        };
        if field_owner != owner
            || !declared
                .iter()
                .any(|field| field.name == name && field.desc == desc)
            || at < 2
        {
            continue;
        }
        let (
            Node::Insn(Insn::Var { op: ALOAD, slot: 0 }),
            Node::Insn(Insn::Var { op: load, slot }),
        ) = (&body.nodes[at - 2], &body.nodes[at - 1])
        else {
            continue;
        };
        let from_parameter = parameters
            .get(slot)
            .is_some_and(|parameter| parameter == desc);
        if *load != load_opcode(desc) || !from_parameter {
            continue;
        }
        if captured.iter().any(|field| field.name == *name) {
            return Err(unsupported("a captured field stored twice"));
        }
        captured.push(CapturedField {
            name: name.clone(),
            desc: desc.clone(),
            slot: *slot,
        });
        remove.extend([at - 2, at - 1, at]);
    }
    if captured.len() != declared.len() {
        return Err(unsupported(
            "a captured field its constructor does not store",
        ));
    }
    for at in remove.into_iter().rev() {
        body.nodes.remove(at);
    }
    // Any other access to a captured field is remapped by kotlinc's field remapper, which this
    // stage does not port.
    let accesses_captured = body.instructions().any(|insn| {
        matches!(insn, Insn::Field { op: GETFIELD | PUTFIELD, name, .. } if is_captured_field_name(name))
    });
    if accesses_captured {
        return Err(unsupported(
            "a constructor that accesses a captured field other than by storing its parameter",
        ));
    }

    captured.sort_by_key(|field| field.slot);
    let desc = format!("({})V", arguments.concat());
    Ok(Constructor {
        desc,
        fields: captured,
        body,
    })
}

impl Constructor {
    /// The new constructor: a label, every captured field's store from its parameter, then the
    /// copied `body`, whose locals that started at its first label now start at the new one.
    pub(super) fn assemble(&self, new_class: &str, mut body: MethodNode) -> MethodNode {
        let old_start = match body.nodes.first() {
            Some(Node::Label(label)) => Some(*label),
            _ => None,
        };
        let start = body.new_label();
        let mut prologue = vec![Node::Label(start)];
        for field in &self.fields {
            prologue.push(Node::Insn(Insn::Var { op: ALOAD, slot: 0 }));
            prologue.push(Node::Insn(Insn::Var {
                op: load_opcode(&field.desc),
                slot: field.slot,
            }));
            prologue.push(Node::Insn(Insn::Field {
                op: PUTFIELD,
                owner: new_class.to_string(),
                name: field.name.clone(),
                desc: field.desc.clone(),
            }));
        }
        body.nodes.splice(0..0, prologue);
        if let Some(old_start) = old_start {
            for local in &mut body.local_variables {
                if local.start == old_start {
                    local.start = start;
                }
            }
        }
        body.desc = self.desc.clone();
        body
    }
}

/// `Type.getOpcode(ILOAD)` for a value of type `desc`.
fn load_opcode(desc: &str) -> u8 {
    match desc.as_bytes()[0] {
        b'J' => 0x16,
        b'F' => 0x17,
        b'D' => 0x18,
        b'L' | b'[' => ALOAD,
        _ => 0x15,
    }
}

/// Each parameter descriptor of the method descriptor `desc`.
fn argument_descriptors(desc: &str) -> Option<Vec<String>> {
    let inner = desc.strip_prefix('(')?.split_once(')')?.0;
    let bytes = inner.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        while bytes.get(at) == Some(&b'[') {
            at += 1;
        }
        match bytes.get(at)? {
            b'L' => at += inner[at..].find(';')? + 1,
            _ => at += 1,
        }
        out.push(inner[start..at].to_string());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "lib/LibKt$greeter$1";
    const STRING: &str = "Ljava/lang/String;";

    fn var(op: u8, slot: u16) -> Node {
        Node::Insn(Insn::Var { op, slot })
    }

    fn put(owner: &str, name: &str) -> Node {
        Node::Insn(Insn::Field {
            op: PUTFIELD,
            owner: owner.to_string(),
            name: name.to_string(),
            desc: STRING.to_string(),
        })
    }

    /// `<init>(String)` of `OWNER`: `stores`, then `super()` and `return`.
    fn constructor(stores: Vec<Node>) -> MethodNode {
        let mut node = MethodNode::new(0, "<init>", "(Ljava/lang/String;)V");
        node.nodes = stores;
        node.nodes.extend([
            var(ALOAD, 0),
            Node::Insn(Insn::Method {
                op: 0xb7,
                owner: "java/lang/Object".to_string(),
                name: "<init>".to_string(),
                desc: "()V".to_string(),
                interface: false,
            }),
            Node::Insn(Insn::Op(0xb1)),
        ]);
        node
    }

    fn prefix() -> [DeclaredCapture<'static>; 1] {
        [DeclaredCapture {
            name: "$prefix",
            desc: STRING,
        }]
    }

    fn extracted(stores: Vec<Node>) -> Result<Vec<CapturedField>, RegenerationError> {
        extract(
            &constructor(stores),
            OWNER,
            &prefix(),
            "(Ljava/lang/String;)V",
        )
        .map(|constructor| constructor.fields)
    }

    #[test]
    fn a_captured_parameter_store_moves_to_the_new_constructor() {
        let plan = extract(
            &constructor(vec![var(ALOAD, 0), var(ALOAD, 1), put(OWNER, "$prefix")]),
            OWNER,
            &prefix(),
            "(Ljava/lang/String;)V",
        )
        .expect("extracts");
        assert_eq!(
            plan.fields,
            [CapturedField {
                name: "$prefix".to_string(),
                desc: STRING.to_string(),
                slot: 1,
            }]
        );
        assert_eq!(plan.body.nodes, constructor(Vec::new()).nodes);
    }

    #[test]
    fn a_captured_store_of_another_value_declines() {
        assert_eq!(
            extracted(vec![
                var(ALOAD, 0),
                Node::Insn(Insn::Op(0x01)),
                put(OWNER, "$prefix")
            ]),
            Err(RegenerationError::Unsupported(
                "a captured field its constructor does not store"
            ))
        );
    }

    #[test]
    fn a_captured_store_through_the_wrong_load_declines() {
        assert_eq!(
            extracted(vec![var(ALOAD, 0), var(0x15, 1), put(OWNER, "$prefix")]),
            Err(RegenerationError::Unsupported(
                "a captured field its constructor does not store"
            ))
        );
    }

    #[test]
    fn a_captured_named_store_on_another_class_declines() {
        assert_eq!(
            extracted(vec![
                var(ALOAD, 0),
                var(ALOAD, 1),
                put(OWNER, "$prefix"),
                var(ALOAD, 0),
                var(ALOAD, 1),
                put("lib/Other", "$prefix"),
            ]),
            Err(RegenerationError::Unsupported(
                "a constructor that accesses a captured field other than by storing its parameter"
            ))
        );
    }

    #[test]
    fn a_captured_field_stored_twice_declines() {
        assert_eq!(
            extracted(vec![
                var(ALOAD, 0),
                var(ALOAD, 1),
                put(OWNER, "$prefix"),
                var(ALOAD, 0),
                var(ALOAD, 1),
                put(OWNER, "$prefix"),
            ]),
            Err(RegenerationError::Unsupported(
                "a captured field stored twice"
            ))
        );
    }
}
