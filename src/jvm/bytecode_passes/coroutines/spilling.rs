//! Saving locals into the continuation across suspension points (`spillVariables` and its helpers
//! in kotlinc's `CoroutineTransformerMethodVisitor.kt`, with `resumePointDependentAnalysis.kt`).
//!
//! A local is spilled at a suspension point when it is alive there, or — kotlinc's
//! `nullOutSpilledCoroutineLocalsUsingStdlibFunction` mode, the default — when a debugger can see
//! it; a dead reference is spilled through `SpillingKt.nullOutSpilledVariable` so the debugger
//! still finds it while nothing keeps it reachable.

use super::super::analysis::{
    analyze_liveness, is_meaningful, stored_slot, BasicValue, ControlFlowGraph, Frame,
    VariableLiveness,
};
use super::super::descriptors;
use super::super::insn_list::EditableMethod;
use super::super::opcodes::*;
use super::markers::{is_fake_local_variable_for_inline, suspend_lambda_parameter_slots};
use super::spilled_types::spilled_variable_field_types;
use super::suspension_points::SuspensionPoint;
use super::{CoroutineError, DeclaredSpillFields, SpillField, SpilledLocal, StateMachineLayout};
use crate::jvm::method_node::{Insn, LabelId, LocalVariable, Node};

const OBJECT: &str = "Ljava/lang/Object;";

/// `SpillableVariable`.
#[derive(Clone, Debug)]
struct SpillableVariable {
    /// The local's type; `None` for a slot holding `null`, which is re-nulled rather than spilled.
    ty: Option<String>,
    /// The field type: every reference is spilled as an `Object`.
    normalized: String,
    field_name: Option<String>,
    slot: u16,
    alive: bool,
}

impl SpillableVariable {
    fn is_null(&self) -> bool {
        self.ty.is_none()
    }

    fn should_spill_null(&self) -> bool {
        self.normalized == OBJECT && !self.alive
    }
}

/// `Type.normalize`: references become `Object`.
fn normalize(descriptor: &str) -> String {
    if descriptors::is_reference(descriptor) {
        OBJECT.to_string()
    } else {
        descriptor.to_string()
    }
}

/// `fieldNameForVar`: the descriptor's first letter, `$`, and the index among its kind.
fn field_name(normalized: &str, index: usize) -> String {
    format!("{}${index}", &normalized[..1])
}

fn var(op: u8, slot: u16) -> Node {
    Node::Insn(Insn::Var { op, slot })
}

/// What the spilling needs to know about the method being transformed.
pub(crate) struct SpillContext<'a> {
    pub owner: &'a str,
    pub continuation_class: &'a str,
    pub continuation_index: u16,
    pub data_index: u16,
    /// `$completion`'s slot in a named function.
    pub completion_slot: Option<u16>,
    pub is_static: bool,
    /// The spill fields the state class declares before spilling (`initialVarsCountByType`).
    pub declared: &'a [DeclaredSpillFields<'a>],
}

impl SpillContext<'_> {
    /// The highest declared index of the spill fields of `normalized` type.
    fn declared_max(&self, normalized: &str) -> Option<usize> {
        self.declared
            .iter()
            .find(|declared| declared.descriptor == normalized)
            .map(|declared| declared.max_index)
    }

    fn field(&self, op: u8, name: &str, descriptor: &str) -> Node {
        Node::Insn(Insn::Field {
            op,
            owner: self.continuation_class.to_string(),
            name: name.to_string(),
            desc: descriptor.to_string(),
        })
    }
}

/// A node position compared with [`EditableMethod::label_order`].
fn position(index: usize) -> isize {
    isize::try_from(index).expect("a position fits isize")
}

/// `findLocalCorrespondingToSpillableVariable`: the entry of `slot` live from `begin` to past
/// `after`.
fn local_across(method: &EditableMethod, slot: u16, begin: usize, after: usize) -> Option<usize> {
    method.method.local_variables.iter().position(|local| {
        local.slot == slot
            && method.label_order(local.start) <= position(begin)
            && method.label_order(local.end) > position(after)
    })
}

/// `spillVariables`: spill and unspill around each point; the continuation's spill fields and each
/// point's spilled locals, for `@DebugMetadata`, are recorded in `layout`.
pub(crate) fn spill_variables(
    method: &mut EditableMethod,
    context: &SpillContext,
    points: &[SuspensionPoint],
    layout: &mut StateMachineLayout,
) -> Result<(), CoroutineError> {
    if points.is_empty() {
        return Ok(());
    }
    let frames = spilled_variable_field_types(method, context.owner)?;
    let suspend_lambda_parameters = suspend_lambda_parameter_slots(&method.insns);
    let snapshot = method.snapshot();
    let liveness = analyze_liveness(&snapshot);

    // Field counts by normalized type, the declared kinds first, then in first-seen order
    // (`maxVarsCountByType`).
    let mut max_counts: Vec<(String, usize)> = context
        .declared
        .iter()
        .map(|declared| (declared.descriptor.to_string(), declared.max_index))
        .collect();
    let mut references = Vec::new();
    let mut primitives = Vec::new();
    let mut all = Vec::new();
    for point in points {
        let after = method
            .insns
            .next(point.end)
            .ok_or(CoroutineError::Unsupported(
                "a suspension point without a following instruction",
            ))?;
        let result_stack = frames[method.insns.index_of(after)]
            .as_ref()
            .ok_or(CoroutineError::Unsupported(
                "an unreachable suspension-point continuation",
            ))?
            .stack
            .len();
        if result_stack != 1 {
            return Err(CoroutineError::InvalidSuspensionResultStack { size: result_stack });
        }
        let begin = method.insns.index_of(point.begin);
        let mut counts: Vec<(String, usize)> = Vec::new();
        let variables =
            variables_to_spill(method, context, &frames, &liveness, begin, &mut counts)?;
        let (refs, prims): (Vec<_>, Vec<_>) = variables
            .iter()
            .cloned()
            .partition(|variable| variable.normalized == OBJECT);
        references.push(refs);
        primitives.push(prims);
        all.push(variables);
        for (ty, index) in counts {
            match max_counts.iter_mut().find(|(seen, _)| *seen == ty) {
                Some((_, max)) => *max = (*max).max(index),
                None => max_counts.push((ty, index)),
            }
        }
    }

    // `initialSpilledVariablesCount`: the reference fields the class declares, which a point
    // without a preceding point is compared against. kotlinc seeds it with the highest declared
    // index instead; the two never differ in output, because every declared parameter field is
    // spilled at every point, so no point spills fewer references than the class declares.
    let initial_references = context.declared_max(OBJECT).map_or(0, |max| max + 1);
    let cleanups = variables_to_clean_up(method, points, &references, initial_references);
    layout.spilled_locals = points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let begin = method.insns.index_of(point.begin);
            references[index]
                .iter()
                .chain(&primitives[index])
                .filter_map(|variable| {
                    let field = variable.field_name.clone()?;
                    let local = local_name(method, variable.slot, begin)?;
                    Some(SpilledLocal { field, local })
                })
                .collect()
        })
        .collect();
    let reinitialized = variables_to_reinitialize(method, points, &all);

    for (index, point) in points.iter().enumerate() {
        for variable in &references[index] {
            spill_and_unspill(method, context, point, variable, &suspend_lambda_parameters);
        }
        let (current, predecessor) = cleanups[index];
        for field in current..predecessor {
            let nodes = vec![
                var(ALOAD, context.continuation_index),
                Node::Insn(Insn::Op(ACONST_NULL)),
                context.field(PUTFIELD, &format!("L${field}"), OBJECT),
            ];
            method.insns.insert_before(point.begin, nodes);
        }
        for variable in &primitives[index] {
            spill_and_unspill(method, context, point, variable, &suspend_lambda_parameters);
        }
        for variable in &reinitialized[index] {
            let zero = match variable.normalized.as_str() {
                "Z" | "B" | "C" | "S" | "I" => Insn::Op(ICONST_0),
                "F" => Insn::Op(FCONST_0),
                "D" => Insn::Op(DCONST_0),
                "J" => Insn::Op(LCONST_0),
                _ => Insn::Op(ACONST_NULL),
            };
            let nodes = vec![
                Node::Insn(zero),
                var(
                    descriptors::typed_opcode(&variable.normalized, ISTORE),
                    variable.slot,
                ),
            ];
            let after = point.try_catch_end(&method.insns);
            method.insns.insert_after(Some(after), nodes);
        }
    }

    for (ty, max) in max_counts {
        let first = context.declared_max(&ty).map_or(0, |declared| declared + 1);
        for index in first..=max {
            layout.fields.push(SpillField {
                name: field_name(&ty, index),
                descriptor: ty.clone(),
            });
        }
    }
    Ok(())
}

/// `calculateVariablesToSpill`.
fn variables_to_spill(
    method: &EditableMethod,
    context: &SpillContext,
    frames: &[Option<Frame<BasicValue>>],
    liveness: &[VariableLiveness],
    begin: usize,
    counts: &mut Vec<(String, usize)>,
) -> Result<Vec<SpillableVariable>, CoroutineError> {
    let frame = frames[begin].as_ref().ok_or(CoroutineError::Unsupported(
        "a suspension point in dead code",
    ))?;
    let alive_here = &liveness[begin];
    let mut variables = Vec::new();
    for (slot, value) in frame.locals.iter().enumerate() {
        let slot = u16::try_from(slot).expect("a local slot fits u16");
        if (!context.is_static && slot == 0)
            || slot == context.continuation_index
            || slot == context.data_index
            || context.completion_slot == Some(slot)
            || is_fake_inliner_variable(method, slot, begin)
        {
            continue;
        }
        let Some(descriptor) = value.descriptor() else {
            continue;
        };
        let visible = method.method.local_variables.iter().any(|local| {
            local.slot == slot
                && method.label_order(local.start) < position(begin)
                && position(begin) < method.label_order(local.end)
        });
        let alive = alive_here.is_alive(usize::from(slot));
        let will_be_visible = !alive && !visible && will_become_visible(method, slot, begin);
        if !(alive || visible || will_be_visible) {
            continue;
        }
        if *value == BasicValue::Null {
            variables.push(SpillableVariable {
                ty: None,
                normalized: OBJECT.to_string(),
                field_name: None,
                slot,
                alive,
            });
            continue;
        }
        let normalized = normalize(descriptor);
        let index = match counts.iter_mut().find(|(ty, _)| *ty == normalized) {
            Some((_, count)) => {
                *count += 1;
                *count
            }
            None => {
                counts.push((normalized.clone(), 0));
                0
            }
        };
        variables.push(SpillableVariable {
            ty: Some(descriptor.to_string()),
            field_name: Some(field_name(&normalized, index)),
            normalized,
            slot,
            alive,
        });
    }
    Ok(variables)
}

/// `isFakeInlinerVariable`: whether the first entry of `slot` covering `begin` is an inliner marker.
fn is_fake_inliner_variable(method: &EditableMethod, slot: u16, begin: usize) -> bool {
    method
        .method
        .local_variables
        .iter()
        .find(|local| {
            local.slot == slot
                && method.label_order(local.start) <= position(begin)
                && position(begin) < method.label_order(local.end)
        })
        .is_some_and(|local| is_fake_local_variable_for_inline(&local.name))
}

/// `checkWhetherVariableWillBeVisible`: a later entry of `slot` that reuses the value rather than
/// storing a new one, and describes some instruction.
fn will_become_visible(method: &EditableMethod, slot: u16, begin: usize) -> bool {
    let Some(local) = method
        .method
        .local_variables
        .iter()
        .filter(|local| local.slot == slot && position(begin) < method.label_order(local.start))
        .min_by_key(|local| method.label_order(local.start))
    else {
        return false;
    };
    let start = method.label_index(local.start);
    // An end not placed yet stands at -1: the entry then covers nothing.
    let end = usize::try_from(method.label_order(local.end))
        .unwrap_or(0)
        .max(start);
    let ids = method.insns.ids();
    if (begin..start).any(|index| stored_slot(method.insns.node(ids[index])) == Some(slot)) {
        return false;
    }
    (start..end).any(|index| is_meaningful(method.insns.node(ids[index])))
}

/// `localVariableName`: the name of the entry of `slot` covering `index`.
fn local_name(method: &EditableMethod, slot: u16, index: usize) -> Option<String> {
    method
        .method
        .local_variables
        .iter()
        .find(|local| {
            local.slot == slot
                && method.label_order(local.start) <= position(index)
                && position(index) < method.label_order(local.end)
        })
        .map(|local| local.name.clone())
}

/// `calculateVariablesToCleanup`: for each point, how many reference fields it fills and the most
/// any directly preceding point filled, `initial` without one; the fields in between are nulled.
fn variables_to_clean_up(
    method: &EditableMethod,
    points: &[SuspensionPoint],
    references: &[Vec<SpillableVariable>],
    initial: usize,
) -> Vec<(usize, usize)> {
    let snapshot = method.snapshot();
    let graph = ControlFlowGraph::build(&snapshot, true);
    let ends: Vec<usize> = points
        .iter()
        .map(|point| method.insns.index_of(point.end))
        .collect();
    let count = |index: usize| references[index].iter().filter(|v| !v.is_null()).count();
    points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            // `findSuspensionPointPredecessors`: walk back to the nearest point ends.
            let mut visited = std::collections::HashSet::new();
            let mut stack = vec![method.insns.index_of(point.begin)];
            let mut predecessors = Vec::new();
            while let Some(node) = stack.pop() {
                if !visited.insert(node) {
                    continue;
                }
                if let Some(point) = ends.iter().position(|&end| end == node) {
                    predecessors.push(point);
                    continue;
                }
                stack.extend(graph.predecessors(node).iter().copied());
            }
            let predecessor = predecessors
                .iter()
                .map(|&p| count(p))
                .max()
                .unwrap_or(initial);
            (count(index), predecessor)
        })
        .collect()
}

/// `calculateVariablesToReinitializeBySuspensionPoint`, the depth-first variant kotlinc uses: a
/// slot spilled at one point but neither spilled nor stored on some path from another point's
/// resumption must be given a value on that resumption.
fn variables_to_reinitialize(
    method: &EditableMethod,
    points: &[SuspensionPoint],
    spilled: &[Vec<SpillableVariable>],
) -> Vec<Vec<SpillableVariable>> {
    let snapshot = method.snapshot();
    let graph = ControlFlowGraph::build(&snapshot, true);
    let ends: Vec<usize> = points
        .iter()
        .map(|point| method.insns.index_of(point.end))
        .collect();
    let mut result: Vec<Vec<SpillableVariable>> = vec![Vec::new(); points.len()];
    for (index, point) in points.iter().enumerate() {
        let start = method.insns.index_of(point.begin);
        for variable in &spilled[index] {
            let slot = variable.slot;
            let mut visited = vec![false; snapshot.nodes.len()];
            visited[start] = true;
            let mut stack = vec![start];
            let mut found = Vec::new();
            while let Some(node) = stack.pop() {
                if let Some(end) = ends.iter().position(|&end| end == node) {
                    if end != index {
                        found.push(end);
                    }
                    continue;
                }
                let stores = match &snapshot.nodes[node] {
                    Node::Insn(Insn::Iinc { slot: stored, .. }) => Some(*stored),
                    other => stored_slot(other),
                };
                if stores == Some(slot) {
                    continue;
                }
                for &predecessor in graph.predecessors(node) {
                    if !visited[predecessor] {
                        visited[predecessor] = true;
                        stack.push(predecessor);
                    }
                }
            }
            for other in found {
                if spilled[other].iter().all(|spilled| spilled.slot != slot) {
                    result[other].push(variable.clone());
                }
            }
        }
    }
    result
}

/// `generateSpillAndUnspill`.
fn spill_and_unspill(
    method: &mut EditableMethod,
    context: &SpillContext,
    point: &SuspensionPoint,
    variable: &SpillableVariable,
    suspend_lambda_parameters: &[u16],
) {
    let try_catch_end = point.try_catch_end(&method.insns);
    let local = local_across(
        method,
        variable.slot,
        method.insns.index_of(point.begin),
        method.insns.index_of(try_catch_end),
    );
    let restart = method.method.new_label();

    let Some(ty) = &variable.ty else {
        let mut nodes = vec![
            Node::Insn(Insn::Op(ACONST_NULL)),
            var(ASTORE, variable.slot),
        ];
        if local.is_some() {
            nodes.push(Node::Label(restart));
        }
        method.insns.insert_after(Some(try_catch_end), nodes);
        split_local(method, point, local, restart);
        return;
    };
    let field = variable
        .field_name
        .as_deref()
        .expect("a non-null spilled variable has a field");
    let is_this = !context.is_static && variable.slot == 0;
    if !is_this {
        let mut nodes = vec![
            var(ALOAD, context.continuation_index),
            var(descriptors::typed_opcode(ty, ILOAD), variable.slot),
        ];
        if variable.should_spill_null() {
            nodes.push(Node::Insn(Insn::Method {
                op: INVOKESTATIC,
                owner: "kotlin/coroutines/jvm/internal/SpillingKt".to_string(),
                name: "nullOutSpilledVariable".to_string(),
                desc: "(Ljava/lang/Object;)Ljava/lang/Object;".to_string(),
                interface: false,
            }));
        }
        nodes.push(context.field(PUTFIELD, field, &variable.normalized));
        method.insns.insert_before(point.begin, nodes);
    }
    if !suspend_lambda_parameters.contains(&variable.slot) && !is_this {
        let mut nodes = vec![
            var(ALOAD, context.continuation_index),
            context.field(GETFIELD, field, &variable.normalized),
        ];
        if ty != &variable.normalized && ty.as_str() != OBJECT {
            nodes.push(Node::Insn(Insn::Type {
                op: CHECKCAST,
                class: descriptors::internal_name(ty).to_string(),
            }));
        }
        nodes.push(var(descriptors::typed_opcode(ty, ISTORE), variable.slot));
        if local.is_some() {
            nodes.push(Node::Label(restart));
        }
        method.insns.insert_after(Some(try_catch_end), nodes);
        split_local(method, point, local, restart);
    }
}

/// `splitLvtRecord`: the local is visible up to the state label, and again once restored.
fn split_local(
    method: &mut EditableMethod,
    point: &SuspensionPoint,
    local: Option<usize>,
    restart: LabelId,
) {
    let Some(local) = local else {
        return;
    };
    let entry = &mut method.method.local_variables[local];
    let previous_end = entry.end;
    entry.end = point.state_label;
    let split = LocalVariable {
        name: entry.name.clone(),
        desc: entry.desc.clone(),
        start: restart,
        end: previous_end,
        slot: entry.slot,
    };
    method.method.local_variables.push(split);
}
