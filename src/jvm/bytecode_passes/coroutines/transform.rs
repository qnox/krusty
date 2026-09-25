//! `CoroutineTransformerMethodVisitor.performTransformations`: the passes both modes share, and the
//! named-function mode (`isForNamedFunction = true`). The suspend-lambda mode is in
//! `lambda_mode`.

use std::collections::HashMap;

use super::super::analysis::{computed_max_stack, node_opcode, ControlFlowGraph};
use super::super::descriptors;
use super::super::fix_stack::fix_stack;
use super::super::insn_list::{EditableMethod, NodeId};
use super::super::opcodes::*;
use super::change_boxing::change_boxing;
use super::markers::{is_fake_continuation_marker, is_suspend_marker, SuspendMarker};
use super::redundant_locals::eliminate_redundant_locals;
use super::spilling::{spill_variables, SpillContext};
use super::state_machine::{
    drop_markers, drop_suspension_markers, drop_unbox_inline_class_markers,
    extend_parameter_ranges, extend_suspend_lambda_parameter_ranges, generate_tableswitch,
    initialize_fake_inliner_variables, line_of, next_line, prepare_prelude, previous_line,
    remove_empty_catch_blocks, split_try_catch_blocks, transform_call_and_return_state_label,
    Machine,
};
use super::suspension_points::{collect_suspension_points, SuspensionPoint};
use super::tail_calls::{add_coroutine_suspended_checks, all_suspension_points_are_tail_calls};
use super::uninitialized_stores::process_uninitialized_stores;
use super::{
    CoroutineError, DebugMetadata, DeclaredSpillFields, NamedFunction, StateMachine,
    StateMachineLayout, Transformed,
};
use crate::jvm::method_node::{Insn, MethodNode, Node};

fn finish_method(method: EditableMethod, owner: &str) -> Result<MethodNode, CoroutineError> {
    let mut method = method.finish();
    method.max_stack = computed_max_stack(&method, owner).map_err(CoroutineError::Analysis)?;
    Ok(method)
}

/// Checks the recorded semantic role against the physical ABI without using the descriptor to
/// discover which parameter is `$completion`.
fn validate_completion_slot(method: &MethodNode, recorded: u16) -> Result<(), CoroutineError> {
    let arguments = descriptors::argument_types(&method.desc).ok_or_else(|| {
        CoroutineError::Analysis(super::super::analysis::AnalyzerError {
            index: 0,
            message: format!("malformed method descriptor {}", method.desc),
        })
    })?;
    if arguments.is_empty() {
        return Err(CoroutineError::Unsupported(
            "a named suspend function without a completion parameter",
        ));
    }
    let before_completion = arguments[..arguments.len() - 1]
        .iter()
        .map(|argument| descriptors::size(argument))
        .sum::<usize>();
    let physical = before_completion + usize::from(method.access & 0x0008 == 0);
    let physical = u16::try_from(physical).map_err(|_| {
        CoroutineError::Unsupported("a named suspend function whose parameters exceed 65535 slots")
    })?;
    if recorded != physical {
        return Err(CoroutineError::CompletionSlotMismatch { recorded, physical });
    }
    if recorded >= method.max_locals {
        return Err(CoroutineError::InvalidCompletionSlot {
            slot: recorded,
            max_locals: method.max_locals,
        });
    }
    Ok(())
}

/// `removeFakeContinuationConstructorCall`: the fake continuation built for an inline suspend
/// function's body becomes `null`.
fn remove_fake_continuation_constructor_call(
    method: &mut EditableMethod,
) -> Result<(), CoroutineError> {
    let ids = method.insns.ids();
    let Some(before) = ids.iter().copied().find(|&id| {
        is_suspend_marker(
            &method.insns,
            id,
            SuspendMarker::BeforeFakeContinuationConstructorCall,
        )
    }) else {
        return Ok(());
    };
    let first = method.insns.prev(before).expect("an id push");
    let last = ids
        .iter()
        .copied()
        .find(|&id| {
            is_suspend_marker(
                &method.insns,
                id,
                SuspendMarker::AfterFakeContinuationConstructorCall,
            )
        })
        .ok_or(CoroutineError::Unsupported(
            "a fake continuation constructor call without its closing marker",
        ))?;
    let mut cursor = Some(first);
    while let Some(id) = cursor {
        if id == last {
            break;
        }
        cursor = method.insns.next(id);
        method.insns.remove(id);
    }
    method.insns.set(last, Node::Insn(Insn::Op(ACONST_NULL)));
    Ok(())
}

/// `replaceReturnsUnitMarkersWithPushingUnitOnStack`, for bodies inlined from old bytecode.
fn replace_returns_unit_markers(method: &mut EditableMethod) {
    for id in method.insns.ids() {
        if !is_suspend_marker(&method.insns, id, SuspendMarker::ReturnsUnit) {
            continue;
        }
        let after = method
            .insns
            .next(id)
            .and_then(|next| method.insns.next(next))
            .expect("a returns-unit marker is followed by the closing marker");
        method.insns.insert_after(
            Some(after),
            vec![
                Node::Insn(Insn::Op(POP)),
                Node::Insn(Insn::Field {
                    op: GETSTATIC,
                    owner: "kotlin/Unit".to_string(),
                    name: "INSTANCE".to_string(),
                    desc: "Lkotlin/Unit;".to_string(),
                }),
            ],
        );
        let push = method.insns.prev(id).expect("an id push");
        method.insns.remove(push);
        method.insns.remove(id);
    }
}

/// `replaceFakeContinuationsWithRealOnes`.
fn replace_fake_continuations(method: &mut EditableMethod, continuation: u16) {
    for id in method.insns.ids() {
        if !is_fake_continuation_marker(&method.insns, id) {
            continue;
        }
        let mark = method.insns.prev(id).expect("a mark call");
        let push = method.insns.prev(mark).expect("an id push");
        method.insns.remove(push);
        method.insns.remove(mark);
        method.insns.set(
            id,
            Node::Insn(Insn::Var {
                op: ALOAD,
                slot: continuation,
            }),
        );
    }
}

/// `checkForSuspensionPointInsideMonitor`.
fn check_monitors(
    method: &EditableMethod,
    points: &[SuspensionPoint],
) -> Result<(), CoroutineError> {
    let snapshot = method.snapshot();
    if !snapshot
        .nodes
        .iter()
        .any(|node| node_opcode(node) == Some(MONITORENTER))
    {
        return Ok(());
    }
    let graph = ControlFlowGraph::build(&snapshot, true);
    let mut depth: HashMap<usize, i32> = HashMap::new();
    let mut stack = vec![(0usize, 0i32)];
    while let Some((index, level)) = stack.pop() {
        if depth.contains_key(&index) {
            continue;
        }
        depth.insert(index, level);
        let next = match node_opcode(&snapshot.nodes[index]) {
            Some(MONITORENTER) => level + 1,
            Some(MONITOREXIT) => level - 1,
            _ => level,
        };
        for &successor in graph.successors(index).iter().rev() {
            if !depth.contains_key(&successor) {
                stack.push((successor, next));
            }
        }
    }
    for point in points {
        if depth
            .get(&method.insns.index_of(point.begin))
            .is_some_and(|level| *level > 0)
        {
            return Err(CoroutineError::SuspensionPointInsideMonitor {
                line: line_of(method, previous_line(method, point.begin)),
            });
        }
    }
    Ok(())
}

/// `addLineNumberForSuspensionPointsAtTheSameLine`: a point on the same line as the one before it
/// gets that line again, so each point has its own line number node.
fn add_line_numbers_for_points_on_the_same_line(
    method: &mut EditableMethod,
    points: &[SuspensionPoint],
) {
    for pair in points.windows(2) {
        let mut cursor = method.insns.next(pair[0].end);
        let mut has_line = false;
        while let Some(id) = cursor {
            if id == pair[1].begin {
                break;
            }
            if matches!(method.insns.node(id), Node::Line { .. }) {
                has_line = true;
                break;
            }
            cursor = method.insns.next(id);
        }
        if has_line {
            continue;
        }
        if let Some(line) = line_of(method, previous_line(method, pair[0].begin)) {
            let label = method.method.new_label();
            method.insns.insert_before(
                pair[1].begin,
                vec![Node::Label(label), Node::Line { line, start: label }],
            );
        }
    }
}

/// The first half of `performTransformations`, which both modes run on the marked body: fake
/// continuations become loads of `continuation`, the operand stack is saved around each call, and
/// the suspension points are collected.
pub(super) fn prepare_marked_body(
    method: &mut EditableMethod,
    owner: &str,
    continuation: u16,
) -> Result<Vec<SuspensionPoint>, CoroutineError> {
    remove_fake_continuation_constructor_call(method)?;
    replace_returns_unit_markers(method);
    replace_fake_continuations(method, continuation);
    fix_stack(method, owner).map_err(CoroutineError::FixStack)?;
    let points = collect_suspension_points(method);
    eliminate_redundant_locals(method, owner, &points)?;
    change_boxing(method);
    check_monitors(method, &points)?;
    add_line_numbers_for_points_on_the_same_line(method, &points);
    Ok(points)
}

/// What building the state machine needs beyond the body, in either mode.
pub(super) struct MachineSpec<'a> {
    /// The internal name of the class declaring the method.
    pub owner: &'a str,
    pub source_file: &'a str,
    pub machine: Machine<'a>,
    /// `$completion`'s slot in a named function, which is never spilled.
    pub completion_slot: Option<u16>,
    /// `getLastParameterIndex`: the parameters up to this slot span the whole method again.
    pub last_parameter_slot: u16,
    pub declared_spill_fields: &'a [DeclaredSpillFields<'a>],
}

/// The second half of `performTransformations`, from `splitTryCatchBlocksContainingSuspensionPoint`
/// on: spilling, the states, and the `tableswitch` inserted before `coroutine_start`.
pub(super) fn build_state_machine(
    mut method: EditableMethod,
    mut points: Vec<SuspensionPoint>,
    spec: &MachineSpec,
    coroutine_start: NodeId,
) -> Result<StateMachine, CoroutineError> {
    let owner = spec.owner;
    let machine = &spec.machine;
    for point in &mut points {
        split_try_catch_blocks(&mut method, point);
    }
    process_uninitialized_stores(&mut method)?;

    let mut layout = StateMachineLayout::default();
    let context = SpillContext {
        owner,
        continuation_class: machine.state_class,
        continuation_index: machine.continuation_index,
        data_index: machine.data_index,
        completion_slot: spec.completion_slot,
        is_static: method.method.access & 0x0008 != 0,
        declared: spec.declared_spill_fields,
    };
    spill_variables(&mut method, &context, &points, &mut layout)?;

    let suspend_marker_var = method.method.max_locals;
    method.method.max_locals += 1;

    let lines: Vec<Option<u16>> = points
        .iter()
        .map(|point| line_of(&method, previous_line(&method, point.begin)))
        .collect();
    let next_lines: Vec<Option<u16>> = points
        .iter()
        .map(|point| line_of(&method, next_line(&method, point.end)))
        .collect();

    let mut state_labels = Vec::with_capacity(points.len());
    for (index, point) in points.iter().enumerate() {
        state_labels.push(transform_call_and_return_state_label(
            &mut method,
            machine,
            index as i32 + 1,
            point,
            suspend_marker_var,
            lines[index],
        ));
    }
    generate_tableswitch(
        &mut method,
        machine,
        coroutine_start,
        suspend_marker_var,
        &state_labels,
    );
    initialize_fake_inliner_variables(&mut method, &state_labels);

    drop_suspension_markers(&mut method);
    drop_unbox_inline_class_markers(&mut method, &points);
    remove_empty_catch_blocks(&mut method);
    extend_parameter_ranges(&mut method, spec.last_parameter_slot);
    extend_suspend_lambda_parameter_ranges(&mut method);
    drop_markers(&mut method, &[SuspendMarker::SuspendLambdaParameter]);

    let line_or_none = |line: &Option<u16>| line.map_or(-1, i32::from);
    let debug_metadata = DebugMetadata {
        source_file: spec.source_file.to_string(),
        line_numbers: lines.iter().map(line_or_none).collect(),
        next_line_numbers: next_lines.iter().map(line_or_none).collect(),
        index_to_label: layout
            .spilled_locals
            .iter()
            .enumerate()
            .flat_map(|(label, locals)| std::iter::repeat_n(label as i32, locals.len()))
            .collect(),
        spilled: layout
            .spilled_locals
            .iter()
            .flatten()
            .map(|spilled| spilled.field.clone())
            .collect(),
        local_names: layout
            .spilled_locals
            .iter()
            .flatten()
            .map(|spilled| spilled.local.clone())
            .collect(),
        method_name: method.method.name.clone(),
        class_name: owner.replace('/', "."),
        version: 2,
    };
    Ok(StateMachine {
        method: finish_method(method, owner)?,
        layout,
        debug_metadata,
    })
}

/// Transforms the marked body of a named suspend function the way kotlinc's
/// `CoroutineTransformerMethodVisitor` does.
pub(crate) fn transform_named_function(
    method: MethodNode,
    function: &NamedFunction,
) -> Result<Transformed, CoroutineError> {
    let owner = function.owner;
    let mut method = EditableMethod::new(method);
    let completion = function.completion_slot;
    validate_completion_slot(&method.method, completion)?;

    let points = prepare_marked_body(&mut method, owner, completion)?;

    let coroutine_start = method.insns.first().ok_or(CoroutineError::Unsupported(
        "an empty suspend function body",
    ))?;

    if all_suspension_points_are_tail_calls(&method, owner, &points)? {
        add_coroutine_suspended_checks(&mut method, &points);
        drop_suspension_markers(&mut method);
        drop_unbox_inline_class_markers(&mut method, &points);
        return Ok(Transformed::TailCalls(finish_method(method, owner)?));
    }

    let data_index = method.method.max_locals;
    let continuation_index = data_index + 1;
    method.method.max_locals += 2;
    let machine = Machine {
        state_class: function.continuation_class,
        line_number: function.line_number,
        continuation_index,
        data_index,
    };
    prepare_prelude(&mut method, &machine, function);

    let spec = MachineSpec {
        owner,
        source_file: function.source_file,
        machine,
        completion_slot: Some(completion),
        last_parameter_slot: completion,
        declared_spill_fields: &[],
    };
    let machine = build_state_machine(method, points, &spec, coroutine_start)?;
    Ok(Transformed::StateMachine(Box::new(machine)))
}
