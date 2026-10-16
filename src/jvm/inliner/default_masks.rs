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
//!
//! An omitted inline lambda parameter's default is not stored either (`extractDefaultLambdasInfo`):
//! the arm's `getstatic Lambda.INSTANCE` and its store are deleted, and the call inlines the
//! lambda class's `invoke` wherever the body invokes the parameter ([`super::default_lambda`]).

use std::collections::{HashSet, VecDeque};

use crate::jvm::bytecode_passes::descriptors;
use crate::jvm::method_node::{Constant, Insn, LabelId, MethodNode, Node};

use super::InlineError;

const ICONST_M1: u8 = 0x02;
const ICONST_5: u8 = 0x08;
const BIPUSH: u8 = 0x10;
const SIPUSH: u8 = 0x11;
const ILOAD: u8 = 0x15;
const ALOAD: u8 = 0x19;
const ISTORE: u8 = 0x36;
const IAND: u8 = 0x7e;
const IFEQ: u8 = 0x99;
const IFNULL: u8 = 0xc6;
const NEW: u8 = 0xbb;
const CHECKCAST: u8 = 0xc0;
const GETSTATIC: u8 = 0xb2;
const INVOKESTATIC: u8 = 0xb8;
const ACC_STATIC: u16 = 0x0008;

/// The constant masks one call passes to a `$default` bridge, and where the bridge declares them:
/// `masks.len()` words from `start`, the method handle right after them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DefaultMasks {
    pub start: u16,
    pub masks: Vec<i32>,
    /// Each source parameter's mask ordinal and exact physical local slot, published by the
    /// realized call-site operand plan. Header parsing must not recover this association from the
    /// bridge's store types or positions.
    pub parameters: Vec<(usize, u16)>,
    /// The slots of the omitted parameters that are inline lambda parameters (kotlinc's
    /// `DefaultValueOfInlineParameter` offsets): their defaults are inlined, never stored.
    pub inline_lambdas: Vec<u16>,
}

/// An omitted inline lambda parameter whose default is a lambda class's singleton
/// (`ExtractedDefaultLambda`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExtractedDefaultLambda {
    /// The parameter's declaration slot.
    pub slot: u16,
    /// The lambda class.
    pub class: String,
    /// The default arm marked the class for reification (`needClassReification`).
    pub need_reification: bool,
}

impl DefaultMasks {
    fn handler(&self) -> Option<u16> {
        self.start
            .checked_add(u16::try_from(self.masks.len()).ok()?)
    }

    fn mask_at(&self, slot: u16) -> Option<i32> {
        slot.checked_sub(self.start)
            .and_then(|word| self.masks.get(word as usize))
            .copied()
    }

    fn parameter_for(&self, word: usize, bit: i32) -> Option<u16> {
        let ordinal = word * 32 + bit.trailing_zeros() as usize;
        self.parameters
            .iter()
            .find_map(|&(parameter, slot)| (parameter == ordinal).then_some(slot))
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
    /// The exact source-parameter slot this mask bit names. `None` identifies the handler guard.
    /// Arm validation is deferred until every range has been checked for overlap.
    expected_parameter: Option<u16>,
    /// The parameter slot the guarded code stores, for a mask test.
    parameter: Option<u16>,
}

/// Resolve `node`'s mask header for `masks` and drop the mask words and the handler from its
/// descriptor; the omitted inline lambda parameters whose defaults are extracted are returned. A
/// test whose shape is not kotlinc's is an error, never left to run.
pub(crate) fn expand(
    node: &mut MethodNode,
    masks: &DefaultMasks,
) -> Result<Vec<ExtractedDefaultLambda>, InlineError> {
    validate_contract(node, masks)?;
    let conditions = header_conditions(node, masks)?;
    let label_positions = label_positions(node);
    let mut deleted = vec![false; node.nodes.len()];
    let mut extracted = Vec::new();
    for condition in &conditions {
        let Some(slot) = condition
            .parameter
            .filter(|slot| condition.keeps_default && masks.inline_lambdas.contains(slot))
        else {
            continue;
        };
        let store = label_positions
            .get(condition.target.index())
            .and_then(Option::as_ref)
            .and_then(|position| position.checked_sub(1))
            .ok_or_else(|| malformed("a mask test jumps to a label the body never places"))?;
        if let Some((lambda, first)) = singleton_default(node, slot, condition.jump, store) {
            for entry in deleted.iter_mut().take(store + 1).skip(first) {
                *entry = true;
            }
            extracted.push(lambda);
        }
    }
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
            if let Some(parameter) = condition.parameter {
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
    node.local_variables.retain(|variable| {
        !extracted
            .iter()
            .any(|lambda| lambda.slot == variable.slot && variable.slot < masks.start)
    });
    reject_uninitialized_suffix_reads(node, masks)?;
    drop_mask_parameters(node, masks)?;
    Ok(extracted)
}

/// The default arm `[needClassReification] getstatic Lambda.INSTANCE [checkcast] astore slot`
/// ending at `store`, and where its deleted instructions begin. A lambda class constructed with
/// captured values is not extracted yet: its default stays the stored object.
fn singleton_default(
    node: &MethodNode,
    slot: u16,
    jump: usize,
    store: usize,
) -> Option<(ExtractedDefaultLambda, usize)> {
    let mut instance = store.checked_sub(1)?;
    if matches!(
        node.nodes.get(instance),
        Some(Node::Insn(Insn::Type { op: CHECKCAST, .. }))
    ) {
        instance = instance.checked_sub(1)?;
    }
    if instance <= jump {
        return None;
    }
    let Some(Node::Insn(Insn::Field {
        op: GETSTATIC,
        owner,
        name,
        ..
    })) = node.nodes.get(instance)
    else {
        return None;
    };
    if name != "INSTANCE" {
        return None;
    }
    let marker = instance.checked_sub(1).filter(|&at| {
        at > jump
            && matches!(
                node.nodes.get(at),
                Some(Node::Insn(Insn::Method {
                    op: INVOKESTATIC,
                    owner,
                    name,
                    desc,
                    ..
                })) if owner == "kotlin/jvm/internal/Intrinsics"
                    && name == "needClassReification"
                    && desc == "()V"
            )
    });
    Some((
        ExtractedDefaultLambda {
            slot,
            class: owner.clone(),
            need_reification: marker.is_some(),
        },
        marker.unwrap_or(instance),
    ))
}

/// Validate the call-site contract against the bridge descriptor before inspecting its body. The
/// mask words and marker are an exact final suffix, and every set bit names one distinct omitted
/// parameter at an actual parameter boundary.
fn validate_contract(node: &MethodNode, masks: &DefaultMasks) -> Result<(), InlineError> {
    if masks.masks.is_empty() {
        return Err(malformed("a default bridge has no mask words"));
    }
    let handler = masks
        .handler()
        .ok_or_else(|| malformed("a default bridge's mask slots overflow the JVM local range"))?;
    let arguments = descriptors::argument_types(&node.desc)
        .ok_or_else(|| malformed("a default bridge's descriptor cannot be parsed"))?;
    let mut slot = if node.access & ACC_STATIC == 0 {
        1u16
    } else {
        0u16
    };
    let mut parameters = Vec::with_capacity(arguments.len());
    for argument in arguments {
        parameters.push((slot, argument));
        slot = slot
            .checked_add(descriptors::size(argument) as u16)
            .ok_or_else(|| {
                malformed("a default bridge's parameters overflow the JVM local range")
            })?;
    }
    let Some(suffix) = parameters
        .iter()
        .position(|&(parameter, _)| parameter == masks.start)
    else {
        return Err(malformed(
            "a default bridge's mask suffix does not begin at a parameter boundary",
        ));
    };
    let expected = masks.masks.len() + 1;
    if parameters.len() - suffix != expected
        || parameters[suffix..parameters.len() - 1]
            .iter()
            .any(|&(_, argument)| argument != "I")
        || parameters.last() != Some(&(handler, "Ljava/lang/Object;"))
    {
        return Err(malformed(
            "a default bridge does not declare the exact mask-and-handler suffix",
        ));
    }

    let mut ordinals = HashSet::new();
    let mut slots = HashSet::new();
    for &(ordinal, parameter) in &masks.parameters {
        let represented = ordinal / 32 < masks.masks.len();
        let boundary = parameters
            .iter()
            .any(|&(slot, _)| slot == parameter && slot < masks.start);
        if !represented || !boundary || !ordinals.insert(ordinal) || !slots.insert(parameter) {
            return Err(malformed(
                "a default call's source-parameter mapping is invalid",
            ));
        }
    }
    if masks
        .masks
        .iter()
        .enumerate()
        .flat_map(|(word, &bits)| {
            (0..32).filter_map(move |bit| {
                (bits & (1i32 << bit) != 0).then_some(word * 32 + bit as usize)
            })
        })
        .any(|ordinal| !ordinals.contains(&ordinal))
    {
        return Err(malformed(
            "a default call's source-parameter mapping is invalid",
        ));
    }
    Ok(())
}

/// kotlinc's `maskProcessingHeader` and its conditions: the instructions before the body first
/// writes a mask slot or the handler slot, which a bridge reuses for its own locals.
fn header_conditions(
    node: &MethodNode,
    masks: &DefaultMasks,
) -> Result<Vec<Condition>, InlineError> {
    let handler = masks
        .handler()
        .ok_or_else(|| malformed("a default bridge's mask slots overflow the JVM local range"))?;
    let label_positions = label_positions(node);
    let mut conditions = Vec::new();
    let mut recognized_bits = vec![0i32; masks.masks.len()];
    let mut previous_end = 0usize;
    let mut handler_seen = false;
    for (at, entry) in node.nodes.iter().enumerate() {
        let Node::Insn(Insn::Var { op, slot }) = entry else {
            continue;
        };
        if let Some(mask) = masks.mask_at(*slot) {
            if *op != ILOAD {
                if is_store(*op) {
                    break;
                }
                return Err(malformed("a mask slot is not read with ILOAD"));
            }
            let (
                Some(Node::Insn(Insn::Op(IAND))),
                Some(Node::Insn(Insn::Jump { op: IFEQ, target })),
            ) = (node.nodes.get(at + 2), node.nodes.get(at + 3))
            else {
                return Err(malformed("a mask test is not followed by IAND and IFEQ"));
            };
            let bit = node
                .nodes
                .get(at + 1)
                .and_then(int_constant)
                .ok_or_else(|| malformed("a mask test does not compare a constant bit"))?;
            if bit.count_ones() != 1 {
                return Err(malformed("a mask test does not compare exactly one bit"));
            }
            let word = usize::from(*slot - masks.start);
            if recognized_bits[word] & bit != 0 {
                return Err(malformed("a default mask bit is tested more than once"));
            }
            recognized_bits[word] |= bit;
            let target_position = label_positions
                .get(target.index())
                .and_then(Option::as_ref)
                .copied()
                .ok_or_else(|| malformed("a mask test jumps to a label the body never places"))?;
            if target_position <= at + 3 {
                return Err(malformed("a mask test does not guard a forward body"));
            }
            if at < previous_end {
                return Err(malformed("default-mask conditions overlap"));
            }
            previous_end = target_position;
            let keeps_default = mask & bit != 0;
            let expected_parameter = masks
                .parameter_for(word, bit)
                .ok_or_else(|| malformed("a mask test names no source parameter"))?;
            conditions.push(Condition {
                first: at,
                jump: at + 3,
                target: *target,
                keeps_default,
                expected_parameter: Some(expected_parameter),
                parameter: None,
            });
        } else if *slot == handler {
            if *op != ALOAD {
                if is_store(*op) {
                    break;
                }
                return Err(malformed("a default-bridge handler is not read with ALOAD"));
            }
            let Some(Node::Insn(Insn::Jump { op: IFNULL, target })) = node.nodes.get(at + 1) else {
                return Err(malformed(
                    "a default-bridge handler load is not followed by its exact throw guard",
                ));
            };
            validate_handler_guard(node, at, *target, &label_positions)?;
            let target_position =
                label_positions[target.index()].expect("the exact handler guard places its target");
            if handler_seen {
                return Err(malformed(
                    "the default-bridge handler is tested more than once",
                ));
            }
            if at < previous_end {
                return Err(malformed("default-mask conditions overlap"));
            }
            handler_seen = true;
            previous_end = target_position;
            conditions.push(Condition {
                first: at,
                jump: at + 1,
                target: *target,
                keeps_default: false,
                expected_parameter: None,
                parameter: None,
            });
        }
    }
    if masks
        .masks
        .iter()
        .zip(recognized_bits)
        .any(|(&set, recognized)| set & !recognized != 0)
    {
        return Err(malformed(
            "a set default mask bit has no recognized condition",
        ));
    }
    // Check every range before interpreting its final instruction. A containing or crossing
    // condition can otherwise make an inner condition's store look like the outer bit's store.
    for condition in &mut conditions {
        let Some(expected_parameter) = condition.expected_parameter else {
            continue;
        };
        let target_position = label_positions
            .get(condition.target.index())
            .and_then(Option::as_ref)
            .copied()
            .ok_or_else(|| malformed("a mask test jumps to a label the body never places"))?;
        let guarded_end = target_position
            .checked_sub(1)
            .and_then(|position| node.nodes.get(position));
        condition.parameter = match guarded_end {
            Some(Node::Insn(Insn::Var {
                op: store_op,
                slot: parameter,
            })) => {
                if *parameter >= masks.start
                    || parameter_store_opcode(node, *parameter) != Some(*store_op)
                {
                    return Err(malformed(
                        "a mask test does not guard the typed store of its parameter",
                    ));
                }
                if expected_parameter != *parameter {
                    return Err(malformed(
                        "a default mask bit does not store its recorded parameter",
                    ));
                }
                Some(*parameter)
            }
            Some(Node::Insn(Insn::Op(0xac..=0xb1 | 0xbf))) => None,
            _ => {
                return Err(malformed(
                    "a mask test guards neither a parameter store nor a terminating arm",
                ));
            }
        };
    }
    Ok(conditions)
}

/// The nullable handler's complete guard. Checking only its first `NEW` would let deletion skip an
/// arbitrary region or preserve a use of the marker after its descriptor parameter was removed.
fn validate_handler_guard(
    node: &MethodNode,
    at: usize,
    target: LabelId,
    positions: &[Option<usize>],
) -> Result<(), InlineError> {
    const DUP: u8 = 0x59;
    const INVOKESPECIAL: u8 = 0xb7;
    const ATHROW: u8 = 0xbf;
    let target_position = positions
        .get(target.index())
        .and_then(Option::as_ref)
        .copied();
    let exact = matches!(
        (
            node.nodes.get(at + 2),
            node.nodes.get(at + 3),
            node.nodes.get(at + 4),
            node.nodes.get(at + 5),
            node.nodes.get(at + 6),
        ),
        (
            Some(Node::Insn(Insn::Type { op: NEW, class })),
            Some(Node::Insn(Insn::Op(DUP))),
            Some(Node::Insn(Insn::Ldc(Constant::String(_)))),
            Some(Node::Insn(Insn::Method {
                op: INVOKESPECIAL,
                owner,
                name,
                desc,
                interface: false,
            })),
            Some(Node::Insn(Insn::Op(ATHROW))),
        ) if class == "java/lang/UnsupportedOperationException"
            && owner == class
            && name == "<init>"
            && desc == "(Ljava/lang/String;)V"
    ) && target_position == Some(at + 7)
        && matches!(node.nodes.get(at + 7), Some(Node::Label(label)) if *label == target);
    if !exact {
        return Err(malformed(
            "a default-bridge handler load is not followed by its exact throw guard",
        ));
    }
    Ok(())
}

fn is_store(op: u8) -> bool {
    matches!(op, 0x36..=0x3a)
}

/// Once the mask header is specialized, its removed parameters must have no surviving read before
/// the body explicitly reuses their slots as locals.
fn reject_uninitialized_suffix_reads(
    node: &MethodNode,
    masks: &DefaultMasks,
) -> Result<(), InlineError> {
    let handler = masks
        .handler()
        .ok_or_else(|| malformed("a default bridge's mask slots overflow the JVM local range"))?;
    if node.nodes.is_empty() {
        return Ok(());
    }
    let labels = label_positions(node);
    let mut states = vec![None; node.nodes.len()];
    states[0] = Some(vec![false; usize::from(handler - masks.start) + 1]);
    let mut queue = VecDeque::from([0usize]);
    while let Some(at) = queue.pop_front() {
        let mut after = states[at]
            .clone()
            .expect("the worklist contains only reached nodes");
        match &node.nodes[at] {
            Node::Insn(Insn::Var { op, slot }) if matches!(*op, 0x15..=0x19 | 0xa9) => {
                let width = u16::from(matches!(*op, 0x16 | 0x18)) + 1;
                require_initialized(&after, *slot, width, masks.start, handler)?;
            }
            Node::Insn(Insn::Var { op, slot }) if is_store(*op) => {
                let width = u16::from(matches!(*op, 0x37 | 0x39)) + 1;
                for written in *slot..slot.saturating_add(width) {
                    if (masks.start..=handler).contains(&written) {
                        after[usize::from(written - masks.start)] = true;
                    }
                }
            }
            Node::Insn(Insn::Iinc { slot, .. }) => {
                require_initialized(&after, *slot, 1, masks.start, handler)?;
            }
            _ => {}
        }

        let before = states[at]
            .clone()
            .expect("the worklist contains only reached nodes");
        for block in &node.try_catch_blocks {
            let (Some(start), Some(end), Some(catch)) = (
                labels.get(block.start.index()).and_then(Option::as_ref),
                labels.get(block.end.index()).and_then(Option::as_ref),
                labels.get(block.handler.index()).and_then(Option::as_ref),
            ) else {
                return Err(malformed("the expanded body has an unplaced handler label"));
            };
            if (*start..*end).contains(&at) {
                merge_initialization(&mut states, &mut queue, *catch, &before)?;
            }
        }
        for successor in normal_successors(node, at, &labels)? {
            merge_initialization(&mut states, &mut queue, successor, &after)?;
        }
    }
    Ok(())
}

fn require_initialized(
    initialized: &[bool],
    slot: u16,
    width: u16,
    start: u16,
    end: u16,
) -> Result<(), InlineError> {
    for read in slot..slot.saturating_add(width) {
        if (start..=end).contains(&read) && !initialized[usize::from(read - start)] {
            return Err(malformed(
                "the expanded body still reads a removed mask or handler parameter",
            ));
        }
    }
    Ok(())
}

fn merge_initialization(
    states: &mut [Option<Vec<bool>>],
    queue: &mut VecDeque<usize>,
    successor: usize,
    incoming: &[bool],
) -> Result<(), InlineError> {
    let Some(state) = states.get_mut(successor) else {
        return Err(malformed(
            "the expanded body has a control-flow edge outside its code",
        ));
    };
    let changed = match state {
        Some(current) => {
            let mut changed = false;
            for (initialized, &incoming) in current.iter_mut().zip(incoming) {
                let merged = *initialized && incoming;
                changed |= merged != *initialized;
                *initialized = merged;
            }
            changed
        }
        None => {
            *state = Some(incoming.to_vec());
            true
        }
    };
    if changed {
        queue.push_back(successor);
    }
    Ok(())
}

fn normal_successors(
    node: &MethodNode,
    at: usize,
    labels: &[Option<usize>],
) -> Result<Vec<usize>, InlineError> {
    let target = |label: LabelId| {
        labels
            .get(label.index())
            .and_then(Option::as_ref)
            .copied()
            .ok_or_else(|| malformed("the expanded body has an unplaced branch label"))
    };
    let next = || (at + 1 < node.nodes.len()).then_some(at + 1);
    match &node.nodes[at] {
        Node::Insn(Insn::Jump {
            op: 0xa7,
            target: branch,
        }) => Ok(vec![target(*branch)?]),
        Node::Insn(Insn::Jump { target: branch, .. }) => {
            Ok(next().into_iter().chain([target(*branch)?]).collect())
        }
        Node::Insn(Insn::TableSwitch {
            default, labels, ..
        })
        | Node::Insn(Insn::LookupSwitch {
            default, labels, ..
        }) => std::iter::once(default)
            .chain(labels)
            .map(|label| target(*label))
            .collect(),
        Node::Insn(Insn::Op(0xac..=0xb1 | 0xbf)) => Ok(Vec::new()),
        _ => Ok(next().into_iter().collect()),
    }
}

/// The store opcode for the descriptor argument whose first local slot is `slot`.
fn parameter_store_opcode(node: &MethodNode, slot: u16) -> Option<u8> {
    let mut at = if node.access & ACC_STATIC == 0 { 1 } else { 0 };
    for argument in descriptors::argument_types(&node.desc)? {
        if at == slot {
            return Some(descriptors::typed_opcode(argument, ISTORE));
        }
        at += descriptors::size(argument) as u16;
    }
    None
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
    let handler = masks
        .handler()
        .ok_or_else(|| malformed("a default bridge's mask slots overflow the JVM local range"))?;
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
    const DUP: u8 = 0x59;
    const INVOKESPECIAL: u8 = 0xb7;
    const ASTORE: u8 = 0x3a;
    const ISTORE: u8 = 0x36;
    const ATHROW: u8 = 0xbf;
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

    fn masks(bits: i32) -> DefaultMasks {
        DefaultMasks {
            start: 2,
            masks: vec![bits],
            parameters: vec![(0, 0), (1, 1)],
            inline_lambdas: Vec::new(),
        }
    }

    fn insert_handler_guard(node: &mut MethodNode, at: usize) {
        let target = node.new_label();
        node.nodes.splice(
            at..at,
            [
                Node::Insn(Insn::Var { op: ALOAD, slot: 3 }),
                Node::Insn(Insn::Jump { op: IFNULL, target }),
                Node::Insn(Insn::Type {
                    op: NEW,
                    class: "java/lang/UnsupportedOperationException".to_owned(),
                }),
                Node::Insn(Insn::Op(DUP)),
                Node::Insn(Insn::Ldc(Constant::String("no super defaults".into()))),
                Node::Insn(Insn::Method {
                    op: INVOKESPECIAL,
                    owner: "java/lang/UnsupportedOperationException".to_owned(),
                    name: "<init>".to_owned(),
                    desc: "(Ljava/lang/String;)V".to_owned(),
                    interface: false,
                }),
                Node::Insn(Insn::Op(ATHROW)),
                Node::Label(target),
            ],
        );
    }

    #[test]
    fn a_set_bit_keeps_the_default_store_and_a_clear_bit_drops_it() {
        let (mut node, start, after_a, after_b) = bridge();
        expand(&mut node, &masks(1)).expect("kotlinc's mask header expands");
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
    fn the_complete_handler_guard_is_removed() {
        let (mut node, _, _, _) = bridge();
        insert_handler_guard(&mut node, 1);
        expand(&mut node, &masks(3)).expect("kotlinc's exact handler guard expands");
        assert!(!node.instructions().any(|instruction| {
            matches!(
                instruction,
                Insn::Type {
                    op: NEW,
                    class,
                } if class == "java/lang/UnsupportedOperationException"
            )
        }));
    }

    #[test]
    fn a_test_that_guards_no_parameter_store_is_rejected() {
        let (mut node, _, _, _) = bridge();
        node.nodes.remove(6);
        assert_eq!(
            expand(&mut node, &masks(3),),
            Err(InlineError::Analysis(
                "a mask test guards neither a parameter store nor a terminating arm".to_owned()
            ))
        );
    }

    #[test]
    fn a_set_default_arm_that_throws_needs_no_parameter_store() {
        // Keep this at the MethodNode boundary: kotlinc's own call-site inliner crashes while
        // expanding an inline default whose arm ends in ATHROW.
        let (mut node, _, _, _) = bridge();
        node.nodes[6] = Node::Insn(Insn::Op(ATHROW));
        expand(&mut node, &masks(1)).expect("a terminating default arm expands");
        assert_eq!(
            node.instructions().collect::<Vec<_>>(),
            vec![
                &Insn::Op(ACONST_NULL),
                &Insn::Op(ATHROW),
                &Insn::Var { op: ILOAD, slot: 1 },
                &Insn::Op(IRETURN),
            ]
        );
    }

    #[test]
    fn a_mask_load_without_the_iand_ifeq_sequence_is_rejected() {
        let (mut node, _, _, _) = bridge();
        node.nodes[3] = Node::Insn(Insn::Op(ICONST_1));
        assert_eq!(
            expand(&mut node, &masks(3),),
            Err(InlineError::Analysis(
                "a mask test is not followed by IAND and IFEQ".to_owned()
            ))
        );
    }

    #[test]
    fn a_mask_branch_that_ends_with_the_wrong_store_is_rejected() {
        let (mut node, _, _, _) = bridge();
        node.nodes[6] = Node::Insn(Insn::Var { op: ALOAD, slot: 0 });
        assert_eq!(
            expand(&mut node, &masks(3),),
            Err(InlineError::Analysis(
                "a mask test does not guard the typed store of its parameter".to_owned()
            ))
        );
    }

    #[test]
    fn a_handler_load_without_the_ifnull_new_sequence_is_rejected() {
        let (mut node, _, _, after_b) = bridge();
        let at = node
            .nodes
            .iter()
            .position(|entry| *entry == Node::Label(after_b))
            .expect("the bridge places its second mask target")
            + 1;
        node.nodes
            .insert(at, Node::Insn(Insn::Var { op: ALOAD, slot: 3 }));
        assert_eq!(
            expand(&mut node, &masks(3),),
            Err(InlineError::Analysis(
                "a default-bridge handler load is not followed by its exact throw guard".to_owned()
            ))
        );
    }

    #[test]
    fn a_mask_slot_read_with_the_wrong_opcode_is_rejected() {
        let (mut node, _, _, _) = bridge();
        node.nodes[1] = Node::Insn(Insn::Var { op: ALOAD, slot: 2 });
        assert_eq!(
            expand(&mut node, &masks(3),),
            Err(InlineError::Analysis(
                "a mask slot is not read with ILOAD".to_owned()
            ))
        );
    }

    #[test]
    fn a_handler_slot_read_with_the_wrong_opcode_is_rejected() {
        let (mut node, _, _, after_b) = bridge();
        let at = node
            .nodes
            .iter()
            .position(|entry| *entry == Node::Label(after_b))
            .expect("the bridge places its second mask target")
            + 1;
        node.nodes
            .insert(at, Node::Insn(Insn::Var { op: ILOAD, slot: 3 }));
        assert_eq!(
            expand(&mut node, &masks(3),),
            Err(InlineError::Analysis(
                "a default-bridge handler is not read with ALOAD".to_owned()
            ))
        );
    }

    #[test]
    fn a_set_mask_bit_without_a_condition_is_rejected() {
        let (mut node, _, _, _) = bridge();
        node.nodes.drain(1..8);
        assert_eq!(
            expand(&mut node, &masks(1),),
            Err(InlineError::Analysis(
                "a set default mask bit has no recognized condition".to_owned()
            ))
        );
    }

    #[test]
    fn a_set_bit_that_stores_another_same_typed_parameter_is_rejected() {
        let (mut node, _, _, _) = bridge();
        node.desc = "(IIILjava/lang/Object;)I".to_owned();
        node.nodes[6] = Node::Insn(Insn::Var {
            op: ISTORE,
            slot: 0,
        });
        node.nodes[13] = Node::Insn(Insn::Var {
            op: ISTORE,
            slot: 0,
        });
        assert_eq!(
            expand(&mut node, &masks(2)),
            Err(InlineError::Analysis(
                "a default mask bit does not store its recorded parameter".to_owned()
            ))
        );
    }

    #[test]
    fn malformed_header_shapes_fail_closed() {
        enum Mutation {
            MultiBit,
            UnknownParameterBit,
            DuplicateBit,
            BackwardTarget,
            OverlappingConditions,
            TruncatedHandler,
            DuplicateHandler,
            WrongSuffix,
            StaleHandlerRead,
            ConditionalSuffixStore,
            ExtraParameterMapping,
        }
        let cases = [
            (
                "one condition tests several bits",
                Mutation::MultiBit,
                "a mask test does not compare exactly one bit",
            ),
            (
                "a clear condition names no source parameter",
                Mutation::UnknownParameterBit,
                "a mask test names no source parameter",
            ),
            (
                "one bit has two conditions",
                Mutation::DuplicateBit,
                "a default mask bit is tested more than once",
            ),
            (
                "a condition jumps backward",
                Mutation::BackwardTarget,
                "a mask test does not guard a forward body",
            ),
            (
                "one condition contains the next",
                Mutation::OverlappingConditions,
                "default-mask conditions overlap",
            ),
            (
                "the handler guard is only a prefix",
                Mutation::TruncatedHandler,
                "a default-bridge handler load is not followed by its exact throw guard",
            ),
            (
                "the handler is tested twice",
                Mutation::DuplicateHandler,
                "the default-bridge handler is tested more than once",
            ),
            (
                "the descriptor has a non-marker suffix",
                Mutation::WrongSuffix,
                "a default bridge does not declare the exact mask-and-handler suffix",
            ),
            (
                "the transformed body still reads the handler",
                Mutation::StaleHandlerRead,
                "the expanded body still reads a removed mask or handler parameter",
            ),
            (
                "only one branch initializes a reused mask slot",
                Mutation::ConditionalSuffixStore,
                "the expanded body still reads a removed mask or handler parameter",
            ),
            (
                "a source parameter is mapped twice",
                Mutation::ExtraParameterMapping,
                "a default call's source-parameter mapping is invalid",
            ),
        ];
        for (case, mutation, expected) in cases {
            let (mut node, start, _, after_b) = bridge();
            let mut call_masks = masks(3);
            match mutation {
                Mutation::MultiBit => {
                    node.nodes[2] = Node::Insn(Insn::Int {
                        op: BIPUSH,
                        operand: 3,
                    });
                }
                Mutation::UnknownParameterBit => {
                    call_masks = masks(1);
                    node.nodes[9] = Node::Insn(Insn::Op(0x07));
                }
                Mutation::DuplicateBit => node.nodes[9] = Node::Insn(Insn::Op(ICONST_1)),
                Mutation::BackwardTarget => {
                    node.nodes[4] = Node::Insn(Insn::Jump {
                        op: IFEQ,
                        target: start,
                    });
                }
                Mutation::OverlappingConditions => {
                    call_masks = masks(2);
                    node.nodes[4] = Node::Insn(Insn::Jump {
                        op: IFEQ,
                        target: after_b,
                    });
                }
                Mutation::TruncatedHandler => {
                    let at = node
                        .nodes
                        .iter()
                        .position(|entry| *entry == Node::Label(after_b))
                        .expect("the bridge places its second mask target")
                        + 1;
                    node.nodes.splice(
                        at..at,
                        [
                            Node::Insn(Insn::Var { op: ALOAD, slot: 3 }),
                            Node::Insn(Insn::Jump {
                                op: IFNULL,
                                target: after_b,
                            }),
                            Node::Insn(Insn::Type {
                                op: NEW,
                                class: "java/lang/UnsupportedOperationException".to_owned(),
                            }),
                        ],
                    );
                }
                Mutation::DuplicateHandler => {
                    insert_handler_guard(&mut node, 1);
                    insert_handler_guard(&mut node, 9);
                }
                Mutation::WrongSuffix => {
                    node.desc = "(Ljava/lang/String;IILjava/lang/String;)I".to_owned();
                }
                Mutation::StaleHandlerRead => {
                    let at = node
                        .nodes
                        .iter()
                        .position(|entry| *entry == Node::Label(after_b))
                        .expect("the bridge places its second mask target")
                        + 1;
                    node.nodes.splice(
                        at..at,
                        [
                            Node::Insn(Insn::Op(ICONST_1)),
                            Node::Insn(Insn::Var {
                                op: ISTORE,
                                slot: 2,
                            }),
                            Node::Insn(Insn::Var { op: ALOAD, slot: 3 }),
                        ],
                    );
                }
                Mutation::ConditionalSuffixStore => {
                    let skip = node.new_label();
                    let at = node
                        .nodes
                        .iter()
                        .position(|entry| *entry == Node::Label(after_b))
                        .expect("the bridge places its second mask target")
                        + 1;
                    node.nodes.splice(
                        at..at,
                        [
                            Node::Insn(Insn::Op(ICONST_1)),
                            Node::Insn(Insn::Jump {
                                op: IFEQ,
                                target: skip,
                            }),
                            Node::Insn(Insn::Op(ICONST_1)),
                            Node::Insn(Insn::Var {
                                op: ISTORE,
                                slot: 2,
                            }),
                            Node::Label(skip),
                            Node::Insn(Insn::Var { op: ILOAD, slot: 2 }),
                        ],
                    );
                }
                Mutation::ExtraParameterMapping => {
                    call_masks = masks(1);
                    call_masks.parameters.push((1, 1));
                }
            }
            assert_eq!(
                expand(&mut node, &call_masks),
                Err(InlineError::Analysis(expected.to_owned())),
                "{case}"
            );
        }
    }
}
