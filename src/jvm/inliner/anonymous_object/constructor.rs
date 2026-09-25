//! The regenerated class's constructor: kotlinc's `extractParametersMappingAndPatchConstructor` and
//! `generateConstructorAndFields` for an object none of whose constructor arguments is an inline
//! lambda.
//!
//! The original constructor stores each captured value (`$x`, `this$0`, `receiver$0`) into its
//! field straight from its parameter. Those stores are taken out of the copied body; the new
//! constructor declares the captured fields afresh and stores every one of them first, then runs
//! what is left of the original body.

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

/// Take the captured-field stores out of `original` (`findCapturedFieldAssignmentInstructions`)
/// and plan the new constructor for a call through `call_desc`.
pub(super) fn extract(
    original: &MethodNode,
    call_desc: &str,
) -> Result<Constructor, RegenerationError> {
    let mut body = original.clone();
    // Parameter slot → the captured field it initializes.
    let mut captured: Vec<(u16, CapturedField)> = Vec::new();
    let mut remove = Vec::new();
    for (at, entry) in body.nodes.iter().enumerate() {
        let Node::Insn(Insn::Field {
            op: PUTFIELD,
            name,
            desc,
            ..
        }) = entry
        else {
            continue;
        };
        if !is_captured_field_name(name) || at < 2 {
            continue;
        }
        let (Node::Insn(Insn::Var { slot, .. }), Node::Insn(Insn::Var { slot: 0, .. })) =
            (&body.nodes[at - 1], &body.nodes[at - 2])
        else {
            continue;
        };
        captured.retain(|(known, _)| known != slot);
        captured.push((
            *slot,
            CapturedField {
                name: name.clone(),
                desc: desc.clone(),
                slot: *slot,
            },
        ));
        remove.extend([at - 2, at - 1, at]);
    }
    for at in remove.into_iter().rev() {
        body.nodes.remove(at);
    }
    // A captured field the body still reads is remapped to a local by kotlinc's field remapper,
    // which this stage does not port.
    let reads_captured = body.instructions().any(|insn| {
        matches!(insn, Insn::Field { op: GETFIELD, name, .. } if is_captured_field_name(name))
    });
    if reads_captured {
        return Err(RegenerationError::Unsupported(
            "a constructor that reads a captured field",
        ));
    }

    let arguments = argument_descriptors(call_desc).ok_or(RegenerationError::Unsupported(
        "a malformed constructor descriptor",
    ))?;
    let mut fields = Vec::new();
    let mut slot = 1u16;
    for argument in &arguments {
        if let Some(position) = captured.iter().position(|(known, _)| *known == slot) {
            fields.push(captured.remove(position).1);
        }
        slot += if matches!(argument.as_bytes()[0], b'J' | b'D') {
            2
        } else {
            1
        };
    }
    let desc = format!("({})V", arguments.concat());
    Ok(Constructor { desc, fields, body })
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
