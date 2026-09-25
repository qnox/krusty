//! kotlinc's `RedundantBoxingMethodTransformer`: the boxes the analysis found removable are never
//! made. The boxing goes, every instruction that used the box works on the unboxed value instead,
//! and the local variables that held a box hold the value (moving the slots above a `long` or
//! `double` up by the extra word).

use std::collections::{BTreeMap, BTreeSet};

use super::super::analysis::{analyze, AnalyzerError, Frame, Value};
use super::super::descriptors;
use super::super::insn_list::{EditableMethod, NodeId};
use super::super::opcodes::*;
use super::interpreter::BoxingInterpreter;
use super::recognizers::{self, ValueClasses};
use super::values::{BoxId, BoxingValue, Candidates};
use crate::jvm::bytecode_passes::analysis::{node_opcode, opcode};
use crate::jvm::method_node::{Insn, LabelPositions, LocalVariable, MethodNode, Node};

/// Removes the boxes of `method`, a member of `owner`, that are only ever unboxed again; `true`
/// when any went.
pub(crate) fn eliminate(
    method: &mut MethodNode,
    owner: &str,
    value_classes: &dyn ValueClasses,
) -> Result<bool, AnalyzerError> {
    if !method.instructions().any(|insn| {
        recognizers::is_boxing(insn, value_classes) || recognizers::is_interface_next(insn)
    }) {
        return Ok(false);
    }
    let mut interpreter = BoxingInterpreter::new(value_classes);
    let frames = analyze(method, owner, &mut interpreter)?;
    interpret_pops(&mut interpreter, method, &frames);

    let tainted_iterators = std::mem::take(&mut interpreter.tainted_iterators);
    let mut candidates = std::mem::take(&mut interpreter.candidates);
    if candidates.is_empty() {
        return Ok(false);
    }
    for id in candidates.safe() {
        if candidates.values[id]
            .progression_iterator
            .as_ref()
            .is_some_and(|iterator| tainted_iterators.contains(&iterator.call))
        {
            candidates.remove(id);
        }
    }
    let positions = LabelPositions::of(method);
    while remove_values_clashing_with_variables(&mut candidates, method, &positions, &frames) {}
    retype_local_variables(&candidates, method, &positions, &frames);
    remap_wide_variables(&candidates, method);
    adapt_instructions(&candidates, method)?;
    Ok(true)
}

/// `interpretPopInstructionsForBoxedValues`: a `pop` of a box is one of its uses.
fn interpret_pops(
    interpreter: &mut BoxingInterpreter,
    method: &MethodNode,
    frames: &[Option<Frame<BoxingValue>>],
) {
    for (at, (node, frame)) in method.nodes.iter().zip(frames).enumerate() {
        let Some(op @ (POP | POP2)) = node_opcode(node) else {
            continue;
        };
        let Some(frame) = frame else {
            continue;
        };
        let Some(top) = frame.stack.last() else {
            continue;
        };
        interpreter.process_pop(top, at, op);
        if top.size() == 1 && op == POP2 {
            if let Some(below) = frame.stack.len().checked_sub(2).map(|k| &frame.stack[k]) {
                interpreter.process_pop(below, at, op);
            }
        }
    }
}

/// Whether a local variable's type is a class (ASM's `Type.OBJECT` sort, arrays excluded).
fn is_object(variable: &LocalVariable) -> bool {
    variable.desc.starts_with('L')
}

/// `getValuesStoredOrLoadedToVariable`: the variable's value where its range starts, and every
/// value stored to or loaded from its slot within the range.
fn values_of_variable<'f>(
    variable: &LocalVariable,
    method: &MethodNode,
    positions: &LabelPositions,
    frames: &'f [Option<Frame<BoxingValue>>],
) -> Vec<&'f BoxingValue> {
    let (start, end) = (positions.at(variable.start), positions.at(variable.end));
    let slot = usize::from(variable.slot);
    let mut values = Vec::new();
    if let Some(local) = frames
        .get(start)
        .and_then(Option::as_ref)
        .and_then(|frame| frame.locals.get(slot))
    {
        values.push(local);
    }
    let range = method.nodes.iter().zip(frames).take(end).skip(start);
    for (node, frame) in range {
        let Some(frame) = frame else {
            continue;
        };
        match node {
            Node::Insn(Insn::Var {
                op: ASTORE,
                slot: stored,
            }) if usize::from(*stored) == slot => {
                values.extend(frame.stack.last());
            }
            Node::Insn(Insn::Var {
                op: ALOAD,
                slot: loaded,
            }) if usize::from(*loaded) == slot => {
                values.extend(frame.locals.get(slot));
            }
            _ => {}
        }
    }
    values
}

/// One pass of `removeValuesClashingWithVariables`: a variable that holds a box and anything else
/// (another value, a tainted box, a box of another type) keeps all its boxes. `true` when a box
/// stopped being removable, which can make another variable clash.
fn remove_values_clashing_with_variables(
    candidates: &mut Candidates,
    method: &MethodNode,
    positions: &LabelPositions,
    frames: &[Option<Frame<BoxingValue>>],
) -> bool {
    let mut repeat = false;
    for variable in method.local_variables.iter().filter(|v| is_object(v)) {
        let values = values_of_variable(variable, method, positions, frames);
        let boxed: Vec<BoxId> = values.iter().filter_map(|value| value.boxed()).collect();
        let Some(&first) = boxed.first() else {
            continue;
        };
        let unboxed_type = candidates.values[first].unboxed_type.clone();
        let unsafe_to_remove = values.iter().any(|value| match value {
            _ if value.is_uninitialized() => false,
            BoxingValue::Boxed {
                id, tainted: false, ..
            } => {
                let value = &candidates.values[*id];
                !value.safe_to_remove || value.unboxed_type != unboxed_type
            }
            _ => true,
        });
        if unsafe_to_remove {
            for id in boxed {
                if candidates.values[id].safe_to_remove {
                    candidates.remove(id);
                    repeat = true;
                }
            }
        }
    }
    repeat
}

/// `adaptLocalSingleVariableTableForBoxedValuesAndPrepareMultiVariables`: a variable that held a
/// removed box is declared with the unboxed type.
fn retype_local_variables(
    candidates: &Candidates,
    method: &mut MethodNode,
    positions: &LabelPositions,
    frames: &[Option<Frame<BoxingValue>>],
) {
    let mut retyped = Vec::new();
    for (index, variable) in method.local_variables.iter().enumerate() {
        if !is_object(variable) {
            continue;
        }
        for value in values_of_variable(variable, method, positions, frames) {
            if let Some(id) = value.boxed() {
                let value = &candidates.values[id];
                if value.safe_to_remove {
                    retyped.push((index, value.unboxed_type.clone()));
                }
            }
        }
    }
    for (index, desc) in retyped {
        method.local_variables[index].desc = desc;
    }
}

/// `buildVariablesRemapping` and `remapLocalVariables`: a slot that held a removed box of a `long`
/// or `double` now needs two words, so every slot above it moves up by one.
fn remap_wide_variables(candidates: &Candidates, method: &mut MethodNode) {
    let mut wide: BTreeMap<u16, u16> = BTreeMap::new();
    for id in candidates.safe() {
        let value = &candidates.values[id];
        if descriptors::size(&value.unboxed_type) < 2 {
            continue;
        }
        for &slot in &value.variables {
            wide.insert(slot, 1);
        }
    }
    if wide.is_empty() {
        return;
    }
    let extra: u16 = wide.values().sum();
    method.max_locals += extra;
    let remap = |slot: u16| -> u16 {
        slot + wide
            .iter()
            .filter(|(&wide_slot, _)| wide_slot < slot)
            .map(|(_, &shift)| shift)
            .sum::<u16>()
    };
    for node in &mut method.nodes {
        if let Node::Insn(Insn::Var { slot, .. } | Insn::Iinc { slot, .. }) = node {
            *slot = remap(*slot);
        }
    }
    for variable in &mut method.local_variables {
        variable.slot = remap(variable.slot);
    }
}

fn pop_of(unboxed_type: &str) -> Insn {
    Insn::Op(if descriptors::size(unboxed_type) == 2 {
        POP2
    } else {
        POP
    })
}

fn static_call(owner: &str, name: &str, desc: &str) -> Insn {
    Insn::Method {
        op: INVOKESTATIC,
        owner: owner.to_string(),
        name: name.to_string(),
        desc: desc.to_string(),
        interface: false,
    }
}

/// ASM's `InstructionAdapter.cast` between two primitive types.
fn cast(from: &str, to: &str) -> Vec<Insn> {
    if from == to {
        return Vec::new();
    }
    let op = |op| Insn::Op(op);
    match (from, to) {
        ("D", "F") => vec![op(D2F)],
        ("D", "J") => vec![op(D2L)],
        ("D", _) => [vec![op(D2I)], cast("I", to)].concat(),
        ("F", "D") => vec![op(F2D)],
        ("F", "J") => vec![op(F2L)],
        ("F", _) => [vec![op(F2I)], cast("I", to)].concat(),
        ("J", "D") => vec![op(L2D)],
        ("J", "F") => vec![op(L2F)],
        ("J", _) => [vec![op(L2I)], cast("I", to)].concat(),
        (_, "B") => vec![op(I2B)],
        (_, "C") => vec![op(I2C)],
        (_, "D") => vec![op(I2D)],
        (_, "F") => vec![op(I2F)],
        (_, "J") => vec![op(I2L)],
        (_, "S") => vec![op(I2S)],
        _ => Vec::new(),
    }
}

/// `adaptInstructionsForBoxedValues`: the boxing goes, each unboxing to another type becomes a
/// primitive conversion, and each use is redone on the unboxed value.
fn adapt_instructions(
    candidates: &Candidates,
    method: &mut MethodNode,
) -> Result<(), AnalyzerError> {
    let mut editable = EditableMethod::new(method.clone());
    let ids = editable.insns.ids();
    let mut adapted: BTreeSet<NodeId> = BTreeSet::new();
    for id in candidates.safe() {
        let value = &candidates.values[id];
        let boxing = ids[value.boxing];
        match &value.progression_iterator {
            None => editable.insns.remove(boxing),
            Some(iterator) => {
                editable.insns.insert_before(
                    boxing,
                    vec![Node::Insn(Insn::Type {
                        op: CHECKCAST,
                        class: iterator.internal_name().to_string(),
                    })],
                );
                editable.insns.set(
                    boxing,
                    Node::Insn(Insn::Method {
                        op: INVOKEVIRTUAL,
                        owner: iterator.internal_name().to_string(),
                        name: iterator.next_method_name(),
                        desc: format!("(){}", iterator.primitive()),
                        interface: false,
                    }),
                );
            }
        }
        for (at, ty) in value.unboxings_with_cast() {
            let unboxing = ids[at];
            let conversion = cast(&value.unboxed_type, &ty)
                .into_iter()
                .map(Node::Insn)
                .collect();
            editable.insns.insert_before(unboxing, conversion);
            editable.insns.remove(unboxing);
        }
        for at in value.associated_insns() {
            let node = ids[at];
            if adapted.insert(node) {
                adapt_instruction(&mut editable, node, &value.unboxed_type, at)?;
            }
        }
    }
    *method = editable.finish();
    Ok(())
}

fn cannot_adapt(at: usize, insn: &Insn) -> AnalyzerError {
    AnalyzerError {
        index: at,
        message: format!("cannot adapt {insn:?} to an unboxed value"),
    }
}

/// `adaptInstruction`: one use of a removed box, redone on the unboxed value.
fn adapt_instruction(
    editable: &mut EditableMethod,
    node: NodeId,
    unboxed_type: &str,
    at: usize,
) -> Result<(), AnalyzerError> {
    let Node::Insn(insn) = editable.insns.node(node).clone() else {
        return Err(AnalyzerError {
            index: at,
            message: "a box's use is not an instruction".to_string(),
        });
    };
    let insns = &mut editable.insns;
    match opcode(&insn) {
        POP => insns.set(node, Node::Insn(pop_of(unboxed_type))),
        DUP => {
            if descriptors::size(unboxed_type) == 2 {
                insns.set(node, Node::Insn(Insn::Op(DUP2)));
            }
        }
        op @ (ASTORE | ALOAD) => {
            let Insn::Var { slot, .. } = insn else {
                return Err(cannot_adapt(at, &insn));
            };
            let base = if op == ASTORE { ISTORE } else { ILOAD };
            insns.set(
                node,
                Node::Insn(Insn::Var {
                    op: descriptors::typed_opcode(unboxed_type, base),
                    slot,
                }),
            );
        }
        INSTANCEOF => {
            insns.insert_before(node, vec![Node::Insn(pop_of(unboxed_type))]);
            insns.set(node, Node::Insn(Insn::Op(ICONST_1)));
        }
        INVOKESTATIC if recognizers::is_are_equal(&insn) => {
            adapt_are_equal(editable, node, unboxed_type, at)?;
        }
        INVOKESTATIC
            if recognizers::is_class_boxing_insn(&insn)
                || recognizers::is_class_unboxing(&insn) =>
        {
            insns.remove(node);
        }
        INVOKEINTERFACE if recognizers::is_comparable_compare_to(&insn) => {
            let replacement = match unboxed_type {
                "Z" | "B" | "S" | "I" | "C" => {
                    static_call("kotlin/jvm/internal/Intrinsics", "compare", "(II)I")
                }
                "J" => Insn::Op(LCMP),
                "F" => static_call("java/lang/Float", "compare", "(FF)I"),
                "D" => static_call("java/lang/Double", "compare", "(DD)I"),
                _ => return Err(cannot_adapt(at, &insn)),
            };
            insns.set(node, Node::Insn(replacement));
        }
        CHECKCAST | INVOKEVIRTUAL => insns.remove(node),
        _ => return Err(cannot_adapt(at, &insn)),
    }
    Ok(())
}

/// `adaptAreEqualIntrinsic`: two unboxed ints (or longs) compare directly, fused with the branch
/// on the result when one follows.
fn adapt_are_equal(
    editable: &mut EditableMethod,
    node: NodeId,
    unboxed_type: &str,
    at: usize,
) -> Result<(), AnalyzerError> {
    // kotlinc's `ifEqualOpcode` and `ifNotEqualOpcode` of the fused branch, and the jump of the
    // 1-or-0 result.
    let (if_eq_opcode, if_ne_opcode, if_differ) = match unboxed_type {
        "Z" | "B" | "S" | "I" | "C" => (IF_ICMPNE, IF_ICMPEQ, IF_ICMPNE),
        "J" => {
            editable
                .insns
                .insert_before(node, vec![Node::Insn(Insn::Op(LCMP))]);
            (IFNE, IFEQ, IFNE)
        }
        // A value class over a reference: its values still compare with `areEqual`.
        _ if descriptors::is_reference(unboxed_type) && !unboxed_type.starts_with('[') => {
            return Ok(())
        }
        _ => {
            return Err(AnalyzerError {
                index: at,
                message: format!("unexpected unboxed type {unboxed_type} for areEqual"),
            })
        }
    };
    let next = editable.insns.next(node);
    let branch = next.and_then(|next| match editable.insns.node(next) {
        Node::Insn(Insn::Jump { op, target }) if *op == IFEQ || *op == IFNE => {
            Some((next, *op, *target))
        }
        _ => None,
    });
    match branch {
        // `fuseAreEqualWithBranch`: `ifeq` jumps when the values differ, `ifne` when they are
        // equal.
        Some((next, op, target)) => {
            let fused = if op == IFEQ {
                if_eq_opcode
            } else {
                if_ne_opcode
            };
            editable
                .insns
                .insert_before(node, vec![Node::Insn(Insn::Jump { op: fused, target })]);
            editable.insns.remove(node);
            editable.insns.remove(next);
        }
        // `ifEqual1Else0`.
        None => {
            let not_equal = editable.method.new_label();
            let done = editable.method.new_label();
            editable.insns.insert_before(
                node,
                vec![
                    Node::Insn(Insn::Jump {
                        op: if_differ,
                        target: not_equal,
                    }),
                    Node::Insn(Insn::Op(ICONST_1)),
                    Node::Insn(Insn::Jump {
                        op: GOTO,
                        target: done,
                    }),
                    Node::Label(not_equal),
                    Node::Insn(Insn::Op(ICONST_0)),
                    Node::Label(done),
                ],
            );
            editable.insns.remove(node);
        }
    }
    Ok(())
}
