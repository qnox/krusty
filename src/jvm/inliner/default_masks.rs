//! A call that omits arguments of an inline function inlines the function's `$default` bridge, and
//! the masks it passes are constants: kotlinc's `expandMaskConditionsAndUpdateVariableNodes`.
//!
//! The bridge opens with one test per defaulted parameter, `iload mask; <bit>; iand; ifeq skip`,
//! whose guarded code stores the default value. The call's mask decides each test: a set bit keeps
//! the default value's code and drops the test, a clear bit drops both. The method-handle test
//! (`aload handler; ifnull; new …`, the unsupported super-call guard) is always dropped. A
//! parameter whose default is now stored unconditionally is described from the end of that
//! store's test on. The mask words and the handler then stop being parameters of the body
//! (kotlinc's `prepareNode` drops them from its descriptor); their slots are ordinary locals the
//! body may reuse.

use std::collections::HashSet;

use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::method_node::{Constant, Insn, LabelId, MethodNode, Node};

use super::InlineError;

const ICONST_M1: u8 = 0x02;
const ICONST_5: u8 = 0x08;
const BIPUSH: u8 = 0x10;
const SIPUSH: u8 = 0x11;
const ILOAD: u8 = 0x15;
const ALOAD: u8 = 0x19;
const IAND: u8 = 0x7e;
const IFEQ: u8 = 0x99;
const IFNULL: u8 = 0xc6;
const NEW: u8 = 0xbb;

/// The constant masks one call passes to a `$default` bridge, and where the bridge declares them:
/// `masks.len()` words from `start`, the method handle right after them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DefaultMasks {
    pub start: u16,
    pub masks: Vec<i32>,
}

impl DefaultMasks {
    fn handler(&self) -> u16 {
        self.start + self.masks.len() as u16
    }

    fn mask_at(&self, slot: u16) -> Option<i32> {
        slot.checked_sub(self.start)
            .and_then(|word| self.masks.get(word as usize))
            .copied()
    }
}

/// One header test and what the call's mask makes of it.
struct Condition {
    /// The `iload`/`aload` that opens the test.
    first: usize,
    /// The test's conditional jump.
    jump: usize,
    target: LabelId,
    /// Whether the guarded default value is stored (its mask bit is set).
    keeps_default: bool,
    /// The parameter slot the guarded code stores, for a mask test.
    parameter: Option<u16>,
}

/// Resolve `node`'s mask header for `masks` and drop the mask words and the handler from its
/// descriptor. A test whose shape is not kotlinc's is an error, never left to run.
pub(crate) fn expand(node: &mut MethodNode, masks: &DefaultMasks) -> Result<(), InlineError> {
    let conditions = header_conditions(node, masks)?;
    let label_positions = label_positions(node);
    let mut deleted = vec![false; node.nodes.len()];
    for condition in &conditions {
        let end = if condition.keeps_default {
            condition.jump + 1
        } else {
            *label_positions
                .get(condition.target.index())
                .and_then(Option::as_ref)
                .ok_or_else(|| malformed("a mask test jumps to a label the body never places"))?
        };
        for entry in deleted.iter_mut().take(end).skip(condition.first) {
            *entry = true;
        }
        if condition.keeps_default {
            let parameter = condition.parameter.expect("a mask test stores a parameter");
            if let Some(variable) = node
                .local_variables
                .iter_mut()
                .rev()
                .find(|variable| variable.slot < masks.start && variable.slot == parameter)
            {
                variable.start = condition.target;
            }
        }
    }
    let deleted_labels: HashSet<LabelId> = node
        .nodes
        .iter()
        .zip(&deleted)
        .filter_map(|(entry, &gone)| match entry {
            Node::Label(label) if gone => Some(*label),
            _ => None,
        })
        .collect();
    let inside = |label: &LabelId| deleted_labels.contains(label);
    node.local_variables
        .retain(|variable| !(inside(&variable.start) && inside(&variable.end)));
    node.try_catch_blocks
        .retain(|block| !(inside(&block.start) && inside(&block.end)));
    // A deleted label stays where it was: it marks the same position, and nothing outside the
    // deleted code names it.
    let mut index = 0;
    node.nodes.retain(|entry| {
        let keep = !deleted[index] || matches!(entry, Node::Label(_));
        index += 1;
        keep
    });
    drop_mask_parameters(node, masks)
}

/// kotlinc's `maskProcessingHeader` and its conditions: the instructions before the body first
/// writes a mask slot or the handler slot, which a bridge reuses for its own locals.
fn header_conditions(
    node: &MethodNode,
    masks: &DefaultMasks,
) -> Result<Vec<Condition>, InlineError> {
    let handler = masks.handler();
    let label_positions = label_positions(node);
    let mut conditions = Vec::new();
    for (at, entry) in node.nodes.iter().enumerate() {
        let Node::Insn(Insn::Var { op, slot }) = entry else {
            continue;
        };
        if let Some(mask) = masks.mask_at(*slot) {
            if *op != ILOAD {
                break;
            }
            let (
                Some(Node::Insn(Insn::Op(IAND))),
                Some(Node::Insn(Insn::Jump { op: IFEQ, target })),
            ) = (node.nodes.get(at + 2), node.nodes.get(at + 3))
            else {
                continue;
            };
            let bit = node
                .nodes
                .get(at + 1)
                .and_then(int_constant)
                .ok_or_else(|| malformed("a mask test does not compare a constant bit"))?;
            let store = label_positions
                .get(target.index())
                .and_then(Option::as_ref)
                .and_then(|&position| position.checked_sub(1))
                .and_then(|position| node.nodes.get(position));
            let Some(Node::Insn(Insn::Var {
                slot: parameter, ..
            })) = store
            else {
                return Err(malformed(
                    "a mask test does not guard a store of its parameter",
                ));
            };
            conditions.push(Condition {
                first: at,
                jump: at + 3,
                target: *target,
                keeps_default: mask & bit != 0,
                parameter: Some(*parameter),
            });
        } else if *slot == handler {
            if *op != ALOAD {
                break;
            }
            if let (
                Some(Node::Insn(Insn::Jump { op: IFNULL, target })),
                Some(Node::Insn(Insn::Type { op: NEW, .. })),
            ) = (node.nodes.get(at + 1), node.nodes.get(at + 2))
            {
                conditions.push(Condition {
                    first: at,
                    jump: at + 1,
                    target: *target,
                    keeps_default: false,
                    parameter: None,
                });
            }
        }
    }
    Ok(conditions)
}

/// The constant an instruction pushes, for the bits a mask test compares (`getConstant`).
fn int_constant(entry: &Node) -> Option<i32> {
    match entry {
        Node::Insn(Insn::Op(op @ ICONST_M1..=ICONST_5)) => Some(i32::from(*op) - 3),
        Node::Insn(Insn::Int {
            op: BIPUSH | SIPUSH,
            operand,
        }) => Some(*operand),
        Node::Insn(Insn::Ldc(Constant::Int(value))) => Some(*value),
        _ => None,
    }
}

/// Where each label is placed, by label index.
fn label_positions(node: &MethodNode) -> Vec<Option<usize>> {
    let mut positions = vec![None; node.label_count()];
    for (at, entry) in node.nodes.iter().enumerate() {
        if let Node::Label(label) = entry {
            positions[label.index()] = Some(at);
        }
    }
    positions
}

/// kotlinc's `prepareNode` descriptor: the parameters declared in the mask words or the handler are
/// gone.
fn drop_mask_parameters(node: &mut MethodNode, masks: &DefaultMasks) -> Result<(), InlineError> {
    let arguments = descriptors::argument_types(&node.desc)
        .ok_or_else(|| malformed("a default bridge's descriptor cannot be parsed"))?;
    let close = node
        .desc
        .find(')')
        .ok_or_else(|| malformed("a default bridge's descriptor has no parameter list"))?;
    let handler = masks.handler();
    let mut offset = 0u16;
    let mut kept = String::from("(");
    for argument in arguments {
        if !(masks.start..=handler).contains(&offset) {
            kept.push_str(argument);
        }
        offset += descriptors::size(argument) as u16;
    }
    if offset <= handler {
        return Err(malformed(
            "a default bridge declares fewer parameters than its masks need",
        ));
    }
    kept.push_str(&node.desc[close..]);
    node.desc = kept;
    Ok(())
}

fn malformed(detail: &str) -> InlineError {
    InlineError::Analysis(detail.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::method_node::{LocalVariable, MethodNode};

    const ACONST_NULL: u8 = 0x01;
    const ICONST_1: u8 = 0x04;
    const ICONST_2: u8 = 0x05;
    const ASTORE: u8 = 0x3a;
    const ISTORE: u8 = 0x36;
    const IRETURN: u8 = 0xac;

    /// `fun f(a: String? = null, b: Int = 7)`'s bridge: two mask tests, then `return b`.
    fn bridge() -> (MethodNode, LabelId, LabelId, LabelId) {
        let mut node = MethodNode::new(
            0x1008,
            "f$default",
            "(Ljava/lang/String;IILjava/lang/Object;)I",
        );
        let start = node.new_label();
        let after_a = node.new_label();
        let after_b = node.new_label();
        let end = node.new_label();
        node.nodes = vec![
            Node::Label(start),
            Node::Insn(Insn::Var { op: ILOAD, slot: 2 }),
            Node::Insn(Insn::Op(ICONST_1)),
            Node::Insn(Insn::Op(IAND)),
            Node::Insn(Insn::Jump {
                op: IFEQ,
                target: after_a,
            }),
            Node::Insn(Insn::Op(ACONST_NULL)),
            Node::Insn(Insn::Var {
                op: ASTORE,
                slot: 0,
            }),
            Node::Label(after_a),
            Node::Insn(Insn::Var { op: ILOAD, slot: 2 }),
            Node::Insn(Insn::Op(ICONST_2)),
            Node::Insn(Insn::Op(IAND)),
            Node::Insn(Insn::Jump {
                op: IFEQ,
                target: after_b,
            }),
            Node::Insn(Insn::Int {
                op: BIPUSH,
                operand: 7,
            }),
            Node::Insn(Insn::Var {
                op: ISTORE,
                slot: 1,
            }),
            Node::Label(after_b),
            Node::Insn(Insn::Var { op: ILOAD, slot: 1 }),
            Node::Insn(Insn::Op(IRETURN)),
            Node::Label(end),
        ];
        node.local_variables = vec![
            LocalVariable {
                name: "a".into(),
                desc: "Ljava/lang/String;".into(),
                start,
                end,
                slot: 0,
            },
            LocalVariable {
                name: "b".into(),
                desc: "I".into(),
                start,
                end,
                slot: 1,
            },
        ];
        (node, start, after_a, after_b)
    }

    #[test]
    fn a_set_bit_keeps_the_default_store_and_a_clear_bit_drops_it() {
        let (mut node, start, after_a, after_b) = bridge();
        expand(
            &mut node,
            &DefaultMasks {
                start: 2,
                masks: vec![1],
            },
        )
        .expect("kotlinc's mask header expands");
        assert_eq!(node.desc, "(Ljava/lang/String;I)I");
        let instructions: Vec<&Insn> = node.instructions().collect();
        assert_eq!(
            instructions,
            vec![
                &Insn::Op(ACONST_NULL),
                &Insn::Var {
                    op: ASTORE,
                    slot: 0
                },
                &Insn::Var { op: ILOAD, slot: 1 },
                &Insn::Op(IRETURN),
            ]
        );
        assert_eq!(node.local_variables[0].start, after_a);
        assert_eq!(node.local_variables[1].start, start);
        assert!(node.nodes.contains(&Node::Label(after_b)));
    }

    #[test]
    fn a_test_that_guards_no_parameter_store_is_rejected() {
        let (mut node, _, _, _) = bridge();
        node.nodes.remove(6);
        assert_eq!(
            expand(
                &mut node,
                &DefaultMasks {
                    start: 2,
                    masks: vec![3],
                },
            ),
            Err(InlineError::Analysis(
                "a mask test does not guard a store of its parameter".to_owned()
            ))
        );
    }
}
