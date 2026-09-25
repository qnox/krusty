//! The state machine's own code (kotlinc's `CoroutineTransformerMethodVisitor.kt`): the prelude that
//! finds or creates the continuation, the split of protected ranges around each suspension point,
//! each point's state, and the `tableswitch` over the continuation's `label`.

use super::super::analysis::{is_branch_or_call, is_meaningful, node_opcode};
use super::super::descriptors;
use super::super::insn_list::{EditableMethod, NodeId};
use super::super::opcodes::*;
use super::markers::{
    int_constant_insn, is_fake_local_variable_for_inline, is_suspend_marker,
    suspend_lambda_parameter_slots, SuspendMarker,
};
use super::suspension_points::SuspensionPoint;
use super::{coroutine_suspended, NamedFunction};
use crate::jvm::method_node::{Constant, Insn, LabelId, LocalVariable, Node, TryCatchBlock};

const CONTINUATION: &str = "Lkotlin/coroutines/Continuation;";
const OBJECT: &str = "Ljava/lang/Object;";
const ILLEGAL_STATE_ERROR_MESSAGE: &str = "call to 'resume' before 'invoke' with coroutine";

fn var(op: u8, slot: u16) -> Node {
    Node::Insn(Insn::Var { op, slot })
}

fn op(op: u8) -> Node {
    Node::Insn(Insn::Op(op))
}

fn jump(op: u8, target: LabelId) -> Node {
    Node::Insn(Insn::Jump { op, target })
}

/// Where the machine keeps its state: the class declaring `label`, `result` and the spill fields
/// (a named function's continuation class, or the suspend lambda itself), the line its own code is
/// attributed to, and the slots of the continuation and of the resumption result.
pub(crate) struct Machine<'a> {
    pub state_class: &'a str,
    pub line_number: u16,
    pub continuation_index: u16,
    pub data_index: u16,
}

impl Machine<'_> {
    fn label_field(&self, op: u8) -> Node {
        Node::Insn(Insn::Field {
            op,
            owner: self.state_class.to_string(),
            name: "label".to_string(),
            desc: "I".to_string(),
        })
    }

    /// `generateResumeWithExceptionCheck`.
    fn throw_on_failure(&self) -> Vec<Node> {
        vec![
            var(ALOAD, self.data_index),
            Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: "kotlin/ResultKt".to_string(),
                name: "throwOnFailure".to_string(),
                desc: "(Ljava/lang/Object;)V".to_string(),
                interface: false,
            }),
        ]
    }
}

/// `prepareMethodNodePreludeForNamedFunction`.
pub(crate) fn prepare_prelude(
    method: &mut EditableMethod,
    machine: &Machine,
    function: &NamedFunction,
) {
    let completion = function.completion_slot;
    for id in method.insns.ids() {
        if let Node::Insn(Insn::Var { op: ALOAD, slot }) = method.insns.node(id) {
            if *slot == completion {
                method.insns.set(id, var(ALOAD, machine.continuation_index));
            }
        }
    }

    let class = machine.state_class;
    let create = method.method.new_label();
    let created = method.method.new_label();
    let result_start = method.method.new_label();
    let mut nodes = vec![
        var(ALOAD, completion),
        Node::Insn(Insn::Type {
            op: INSTANCEOF,
            class: class.to_string(),
        }),
        jump(IFEQ, create),
        var(ALOAD, completion),
        Node::Insn(Insn::Type {
            op: CHECKCAST,
            class: class.to_string(),
        }),
        var(ASTORE, machine.continuation_index),
        var(ALOAD, machine.continuation_index),
        machine.label_field(GETFIELD),
        Node::Insn(int_constant_insn(i32::MIN)),
        op(IAND),
        jump(IFEQ, create),
        var(ALOAD, machine.continuation_index),
        op(DUP),
        machine.label_field(GETFIELD),
        Node::Insn(int_constant_insn(i32::MIN)),
        op(ISUB),
        machine.label_field(PUTFIELD),
        jump(GOTO, created),
        Node::Label(create),
        Node::Insn(Insn::Type {
            op: NEW,
            class: class.to_string(),
        }),
        op(DUP),
    ];
    // `generateContinuationConstructorCall`.
    let mut constructor = String::from("(");
    if let Some(receiver) = function.dispatch_receiver {
        nodes.push(var(ALOAD, 0));
        constructor.push_str(&descriptors::of_internal_name(receiver));
    }
    nodes.push(var(ALOAD, completion));
    constructor.push_str(CONTINUATION);
    constructor.push_str(")V");
    nodes.extend([
        Node::Insn(Insn::Method {
            op: INVOKESPECIAL,
            owner: class.to_string(),
            name: "<init>".to_string(),
            desc: constructor,
            interface: false,
        }),
        var(ASTORE, machine.continuation_index),
        Node::Label(created),
        var(ALOAD, machine.continuation_index),
        Node::Insn(Insn::Field {
            op: GETFIELD,
            owner: class.to_string(),
            name: "result".to_string(),
            desc: OBJECT.to_string(),
        }),
        var(ASTORE, machine.data_index),
        Node::Label(result_start),
    ]);

    // `addContinuationAndResultToLvt`, which runs while the prelude is built: the end label goes at
    // the end of the body as it is then.
    let end = method.method.new_label();
    method.insns.add(Node::Label(end));
    method.method.local_variables.push(LocalVariable {
        name: "$continuation".to_string(),
        desc: CONTINUATION.to_string(),
        start: created,
        end,
        slot: machine.continuation_index,
    });
    method.method.local_variables.push(LocalVariable {
        name: "$result".to_string(),
        desc: OBJECT.to_string(),
        start: result_start,
        end,
        slot: machine.data_index,
    });
    method.insns.insert_after(None, nodes);
}

/// `splitTryCatchBlocksContainingSuspensionPoint`: `L1: nop L2` after the point; a range around
/// it ends at `L1` and resumes at `L2`, so restored locals are not in any range.
pub(crate) fn split_try_catch_blocks(method: &mut EditableMethod, point: &mut SuspensionPoint) {
    let begin = method.insns.index_of(point.begin);
    let end = method.insns.index_of(point.end);
    let first = method.method.new_label();
    let second = method.method.new_label();
    let ids = method.insns.insert_after(
        Some(point.end),
        vec![Node::Label(first), op(NOP), Node::Label(second)],
    );
    let mut blocks = Vec::new();
    for block in std::mem::take(&mut method.method.try_catch_blocks) {
        let start = method.label_index(block.start);
        let stop = method.label_index(block.end);
        if start < begin && begin < stop {
            debug_assert!(start < end && end < stop);
            blocks.push(TryCatchBlock {
                end: first,
                ..block.clone()
            });
            blocks.push(TryCatchBlock {
                start: second,
                ..block
            });
        } else {
            blocks.push(block);
        }
    }
    method.method.try_catch_blocks = blocks;
    point.try_catch_continuation = Some(ids[2]);
}

/// The line number node nearest before `id` (`findSuspensionPointLineNumber`).
pub(crate) fn previous_line(method: &EditableMethod, id: NodeId) -> Option<NodeId> {
    let mut cursor = method.insns.prev(id);
    while let Some(current) = cursor {
        if matches!(method.insns.node(current), Node::Line { .. }) {
            return Some(current);
        }
        cursor = method.insns.prev(current);
    }
    None
}

/// The line number node nearest after `id` (`findSuspensionPointNextLineNumber`).
pub(crate) fn next_line(method: &EditableMethod, id: NodeId) -> Option<NodeId> {
    let mut cursor = method.insns.next(id);
    while let Some(current) = cursor {
        if matches!(method.insns.node(current), Node::Line { .. }) {
            return Some(current);
        }
        cursor = method.insns.next(current);
    }
    None
}

pub(crate) fn line_of(method: &EditableMethod, id: Option<NodeId>) -> Option<u16> {
    match id.map(|id| method.insns.node(id)) {
        Some(Node::Line { line, .. }) => Some(*line),
        _ => None,
    }
}

/// `nextDefinitelyHitLineNumber`: the next line number before any branch or call.
fn next_definitely_hit_line(method: &EditableMethod, point: &SuspensionPoint) -> Option<NodeId> {
    let mut cursor = method.insns.next(point.end);
    while let Some(id) = cursor {
        let node = method.insns.node(id);
        if is_branch_or_call(node) {
            return None;
        }
        if matches!(node, Node::Line { .. }) {
            return Some(id);
        }
        cursor = method.insns.next(id);
    }
    None
}

/// `transformCallAndReturnStateLabel`: set the label, return `COROUTINE_SUSPENDED` when the call
/// suspends, and resume at the state label with the result the continuation received.
pub(crate) fn transform_call_and_return_state_label(
    method: &mut EditableMethod,
    machine: &Machine,
    id: i32,
    point: &SuspensionPoint,
    suspend_marker_var: u16,
    point_line: Option<u16>,
) -> LabelId {
    let after_loaded_result = method.method.new_label();
    let mut next_line_node = next_definitely_hit_line(method, point);
    method.insns.insert_before(
        point.begin,
        vec![
            var(ALOAD, machine.continuation_index),
            Node::Insn(int_constant_insn(id)),
            machine.label_field(PUTFIELD),
        ],
    );

    let return_label = method.method.new_label();
    let try_catch_end = point.try_catch_end(&method.insns);
    method.insns.insert_after(
        Some(try_catch_end),
        vec![
            op(DUP),
            var(ALOAD, suspend_marker_var),
            jump(IF_ACMPNE, after_loaded_result),
            Node::Label(return_label),
            Node::Line {
                line: machine.line_number,
                start: return_label,
            },
            var(ALOAD, suspend_marker_var),
            op(ARETURN),
            Node::Label(point.state_label),
        ],
    );

    // After the point there are always `L1 nop L2`; the `nop` moves inside the ranges that resume
    // at `L2`.
    let continuation = point
        .try_catch_continuation
        .expect("protected ranges were split around the point");
    let nop = method
        .insns
        .prev(continuation)
        .expect("a nop precedes the continuation label");
    debug_assert_eq!(node_opcode(method.insns.node(nop)), Some(NOP));
    method.insns.remove(nop);

    let mut nodes = vec![op(NOP)];
    nodes.extend(machine.throw_on_failure());
    nodes.push(var(ALOAD, machine.data_index));
    for id in &point.unbox_inline_class {
        nodes.push(method.insns.node(*id).clone());
    }
    nodes.push(Node::Label(after_loaded_result));
    if next_line_node.is_some() {
        // Move the next line up to the resumption, unless the result is used right away.
        let result_used = method.insns.next(continuation).is_some_and(|next| {
            matches!(
                node_opcode(method.insns.node(next)),
                Some(ASTORE | CHECKCAST | INVOKESTATIC | INVOKEVIRTUAL | INVOKEINTERFACE)
            )
        });
        if result_used {
            next_line_node = None;
        } else if let Some(line) = line_of(method, next_line_node) {
            nodes.push(Node::Line {
                line,
                start: after_loaded_result,
            });
        }
    } else if let Some(line) = point_line {
        nodes.push(Node::Line {
            line,
            start: after_loaded_result,
        });
    }
    method.insns.insert_after(Some(continuation), nodes);
    if let Some(line) = next_line_node {
        method.insns.remove(line);
    }
    point.state_label
}

/// `generateStateMachinesTableswitch`.
pub(crate) fn generate_tableswitch(
    method: &mut EditableMethod,
    machine: &Machine,
    coroutine_start: NodeId,
    suspend_marker_var: u16,
    state_labels: &[LabelId],
) {
    let switch_label = method.method.new_label();
    let first_state = method.method.new_label();
    let default = method.method.new_label();
    let line = machine.line_number;
    let mut labels = vec![first_state];
    labels.extend_from_slice(state_labels);
    let mut nodes = vec![
        coroutine_suspended(),
        Node::Label(switch_label),
        Node::Line {
            line,
            start: switch_label,
        },
        var(ASTORE, suspend_marker_var),
        var(ALOAD, machine.continuation_index),
        machine.label_field(GETFIELD),
        Node::Insn(Insn::TableSwitch {
            low: 0,
            high: state_labels.len() as i32,
            default,
            labels,
        }),
        Node::Label(first_state),
    ];
    nodes.extend(machine.throw_on_failure());
    method.insns.insert_before(coroutine_start, nodes);

    let last = method.insns.last();
    method.insns.insert_after(
        last,
        vec![
            Node::Label(default),
            Node::Line {
                line,
                start: default,
            },
            Node::Insn(Insn::Type {
                op: NEW,
                class: "java/lang/IllegalStateException".to_string(),
            }),
            op(DUP),
            Node::Insn(Insn::Ldc(Constant::String(
                ILLEGAL_STATE_ERROR_MESSAGE.into(),
            ))),
            Node::Insn(Insn::Method {
                op: INVOKESPECIAL,
                owner: "java/lang/IllegalStateException".to_string(),
                name: "<init>".to_string(),
                desc: "(Ljava/lang/String;)V".to_string(),
                interface: false,
            }),
            op(ATHROW),
            op(RETURN),
        ],
    );
}

/// `initializeFakeInlinerVariables`: an inliner marker variable spanning a state label is zeroed
/// on resumption, and its entry split so it is defined only once zeroed.
pub(crate) fn initialize_fake_inliner_variables(
    method: &mut EditableMethod,
    state_labels: &[LabelId],
) {
    for &state in state_labels {
        let state_index = method.label_index(state);
        let state_node = method
            .insns
            .label_node(state)
            .expect("a state label is placed");
        let mut added = Vec::new();
        for index in 0..method.method.local_variables.len() {
            let local = &method.method.local_variables[index];
            if !is_fake_local_variable_for_inline(&local.name)
                || method.label_index(local.start) >= state_index
                || state_index >= method.label_index(local.end)
            {
                continue;
            }
            let new_start = method.method.new_label();
            let local = &mut method.method.local_variables[index];
            let old_end = local.end;
            local.end = state;
            let slot = local.slot;
            added.push(LocalVariable {
                name: local.name.clone(),
                desc: local.desc.clone(),
                start: new_start,
                end: old_end,
                slot,
            });
            method.insns.insert_after(
                Some(state_node),
                vec![op(ICONST_0), var(ISTORE, slot), Node::Label(new_start)],
            );
        }
        method.method.local_variables.extend(added);
    }
}

/// Removes each `mark(id)` call for `markers`, with the id push before it.
pub(crate) fn drop_markers(method: &mut EditableMethod, markers: &[SuspendMarker]) {
    for id in method.insns.ids() {
        if markers
            .iter()
            .any(|marker| is_suspend_marker(&method.insns, id, *marker))
        {
            let push = method.insns.prev(id).expect("an id push");
            method.insns.remove(push);
            method.insns.remove(id);
        }
    }
}

/// `dropSuspensionMarkers`.
pub(crate) fn drop_suspension_markers(method: &mut EditableMethod) {
    drop_markers(
        method,
        &[
            SuspendMarker::BeforeSuspend,
            SuspendMarker::AfterSuspend,
            SuspendMarker::BeforeSuspendUnitCall,
            SuspendMarker::BeforeSuspendGenericCall,
        ],
    );
}

/// `dropUnboxInlineClassMarkers`.
pub(crate) fn drop_unbox_inline_class_markers(
    method: &mut EditableMethod,
    points: &[SuspensionPoint],
) {
    drop_markers(method, &[SuspendMarker::BeforeUnboxInlineClass]);
    for id in method.insns.ids() {
        if is_suspend_marker(&method.insns, id, SuspendMarker::AfterUnboxInlineClass) {
            let push = method.insns.prev(id).expect("an id push");
            let before = method.insns.prev(push).expect("a node before the marker");
            method.insns.remove(before);
            method.insns.remove(push);
            method.insns.remove(id);
        }
    }
    for point in points {
        for id in &point.unbox_inline_class {
            method.insns.remove(*id);
        }
    }
}

/// `removeEmptyCatchBlocks`.
pub(crate) fn remove_empty_catch_blocks(method: &mut EditableMethod) {
    let ids = method.insns.ids();
    let blocks = std::mem::take(&mut method.method.try_catch_blocks);
    method.method.try_catch_blocks = blocks
        .into_iter()
        .filter(|block| {
            (method.label_index(block.start)..method.label_index(block.end))
                .any(|index| is_meaningful(method.insns.node(ids[index])))
        })
        .collect();
}

/// `getOrCreateStartingLabel`.
fn starting_label(method: &mut EditableMethod) -> LabelId {
    if let Some(Node::Label(label)) = method.insns.first().map(|id| method.insns.node(id)) {
        return *label;
    }
    let label = method.method.new_label();
    method.insns.insert_after(None, vec![Node::Label(label)]);
    label
}

/// `getOrCreateEndingLabel`.
fn ending_label(method: &mut EditableMethod) -> LabelId {
    if let Some(Node::Label(label)) = method.insns.last().map(|id| method.insns.node(id)) {
        return *label;
    }
    let label = method.method.new_label();
    method.insns.add(Node::Label(label));
    label
}

/// `extendParameterRanges`: the parameters span the whole method again.
pub(crate) fn extend_parameter_ranges(method: &mut EditableMethod, last_parameter_slot: u16) {
    let start = starting_label(method);
    let end = ending_label(method);
    for slot in 0..=last_parameter_slot {
        let Some(first) = method
            .method
            .local_variables
            .iter()
            .position(|local| local.slot == slot)
        else {
            continue;
        };
        method.method.local_variables[first].start = start;
        method.method.local_variables[first].end = end;
        let mut index = 0;
        method.method.local_variables.retain(|local| {
            let keep = local.slot != slot || index == first;
            index += 1;
            keep
        });
    }
}

/// `extendSuspendLambdaParameterRanges`: a suspend lambda's parameters are read from their fields
/// on every entry, before the `tableswitch`, so each spans the method from the first label after
/// the last parameter marker — one entry, moved to the end of the table.
pub(crate) fn extend_suspend_lambda_parameter_ranges(method: &mut EditableMethod) {
    let slots = suspend_lambda_parameter_slots(&method.insns);
    if slots.is_empty() {
        return;
    }
    let Some(last_marker) =
        method.insns.ids().into_iter().rev().find(|&id| {
            is_suspend_marker(&method.insns, id, SuspendMarker::SuspendLambdaParameter)
        })
    else {
        return;
    };
    let mut cursor = method.insns.next(last_marker);
    let start = loop {
        let Some(id) = cursor else {
            return;
        };
        if let Node::Label(label) = method.insns.node(id) {
            break *label;
        }
        cursor = method.insns.next(id);
    };
    for slot in slots {
        let Some(first) = method
            .method
            .local_variables
            .iter()
            .position(|local| local.slot == slot)
        else {
            continue;
        };
        let mut extended = method.method.local_variables[first].clone();
        method
            .method
            .local_variables
            .retain(|local| local.slot != slot);
        extended.start = start;
        extended.end = ending_label(method);
        method.method.local_variables.push(extended);
    }
}
