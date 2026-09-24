//! kotlinc's `TailCallOptimization.kt`: a named suspend function whose every suspension point is a
//! tail call needs no state machine — it returns whatever its callee returned, `COROUTINE_SUSPENDED`
//! included.

use std::collections::HashSet;

use super::super::analysis::{
    analyze, is_meaningful, node_opcode, opcode, AnalyzerError, At, BasicInterpreter, BasicValue,
    ControlFlowGraph, Interpreter, Value,
};
use super::super::insn_list::{EditableMethod, InsnList, NodeId};
use super::super::opcodes::*;
use super::markers::{
    is_inline_marker, is_suspend_inline_marker, is_suspend_marker, SuspendMarker,
};
use super::suspension_points::SuspensionPoint;
use super::{coroutine_suspended, CoroutineError};
use crate::jvm::method_node::{Insn, MethodNode, Node};

/// ASM's plain `BasicValue`s, plus kotlinc's `FromSuspensionPointValue`: a reference a suspension
/// point produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TcoValue {
    Uninitialized,
    Int,
    Float,
    Long,
    Double,
    Reference,
    FromSuspensionPoint,
}

impl Value for TcoValue {
    fn size(&self) -> usize {
        match self {
            TcoValue::Long | TcoValue::Double => 2,
            _ => 1,
        }
    }
}

impl TcoValue {
    fn of(value: &BasicValue) -> TcoValue {
        match value {
            BasicValue::Uninitialized => TcoValue::Uninitialized,
            BasicValue::Int
            | BasicValue::Boolean
            | BasicValue::Char
            | BasicValue::Byte
            | BasicValue::Short => TcoValue::Int,
            BasicValue::Long => TcoValue::Long,
            BasicValue::Float => TcoValue::Float,
            BasicValue::Double => TcoValue::Double,
            BasicValue::Reference(_) | BasicValue::Null => TcoValue::Reference,
        }
    }

    fn representative(self) -> BasicValue {
        match self {
            TcoValue::Uninitialized => BasicValue::Uninitialized,
            TcoValue::Int => BasicValue::Int,
            TcoValue::Float => BasicValue::Float,
            TcoValue::Long => BasicValue::Long,
            TcoValue::Double => BasicValue::Double,
            TcoValue::Reference | TcoValue::FromSuspensionPoint => {
                BasicValue::Reference("Ljava/lang/Object;".to_string())
            }
        }
    }
}

/// kotlinc's `TcoInterpreter`: whatever reference an instruction inside a suspension point
/// produces is marked as coming from it.
struct TcoInterpreter {
    /// The node-index ranges of the suspension points, both ends included.
    ranges: Vec<(usize, usize)>,
}

impl TcoInterpreter {
    fn convert(&self, at: &At, value: Option<TcoValue>) -> Option<TcoValue> {
        let inside = self
            .ranges
            .iter()
            .any(|(begin, end)| (*begin..=*end).contains(&at.index));
        match value {
            Some(TcoValue::Reference) if inside => Some(TcoValue::FromSuspensionPoint),
            other => other,
        }
    }
}

fn kind(value: Option<BasicValue>) -> Option<TcoValue> {
    value.as_ref().map(TcoValue::of)
}

impl Interpreter for TcoInterpreter {
    type V = TcoValue;

    fn new_value(&mut self, ty: Option<&str>) -> Option<TcoValue> {
        kind(BasicInterpreter.new_value(ty))
    }

    fn new_operation(&mut self, at: &At) -> Result<TcoValue, AnalyzerError> {
        let value = kind(Some(BasicInterpreter.new_operation(at)?));
        Ok(self.convert(at, value).expect("a pushed value"))
    }

    fn copy_operation(&mut self, at: &At, value: &TcoValue) -> Result<TcoValue, AnalyzerError> {
        Ok(self.convert(at, Some(*value)).expect("a copied value"))
    }

    fn unary_operation(
        &mut self,
        at: &At,
        value: &TcoValue,
    ) -> Result<Option<TcoValue>, AnalyzerError> {
        if *value == TcoValue::FromSuspensionPoint && opcode(at.insn) == CHECKCAST {
            return Ok(Some(*value));
        }
        let result = kind(BasicInterpreter.unary_operation(at, &value.representative())?);
        Ok(self.convert(at, result))
    }

    fn binary_operation(
        &mut self,
        at: &At,
        first: &TcoValue,
        second: &TcoValue,
    ) -> Result<Option<TcoValue>, AnalyzerError> {
        let result = kind(BasicInterpreter.binary_operation(
            at,
            &first.representative(),
            &second.representative(),
        )?);
        Ok(self.convert(at, result))
    }

    fn ternary_operation(
        &mut self,
        _at: &At,
        _first: &TcoValue,
        _second: &TcoValue,
        _third: &TcoValue,
    ) -> Result<Option<TcoValue>, AnalyzerError> {
        Ok(None)
    }

    fn nary_operation(
        &mut self,
        at: &At,
        values: &[TcoValue],
    ) -> Result<Option<TcoValue>, AnalyzerError> {
        let values: Vec<BasicValue> = values.iter().map(|value| value.representative()).collect();
        let result = kind(BasicInterpreter.nary_operation(at, &values)?);
        Ok(self.convert(at, result))
    }

    fn return_operation(
        &mut self,
        _at: &At,
        _value: &TcoValue,
        _expected: &TcoValue,
    ) -> Result<(), AnalyzerError> {
        Ok(())
    }

    fn merge(&mut self, first: &TcoValue, second: &TcoValue) -> TcoValue {
        if *first == TcoValue::FromSuspensionPoint || *second == TcoValue::FromSuspensionPoint {
            TcoValue::FromSuspensionPoint
        } else if first == second {
            *first
        } else {
            TcoValue::Uninitialized
        }
    }
}

/// `skipUntilMeaningful`: the first meaningful node from `start` on, stepping over `nop`s, over
/// suspend markers when asked, and through `goto`s; `None` on a cycle or at the end.
fn skip_until_meaningful(
    insns: &InsnList,
    start: Option<NodeId>,
    skip_suspend_markers: bool,
) -> Option<NodeId> {
    let mut cursor = start;
    let mut visited = HashSet::new();
    while let Some(id) = cursor {
        if !visited.insert(id) {
            return None;
        }
        let node = insns.node(id);
        let next = insns.next(id);
        cursor = if !is_meaningful(node) || node_opcode(node) == Some(NOP) {
            next
        } else if skip_suspend_markers
            && next.is_some_and(|next| is_suspend_inline_marker(insns.node(next)))
        {
            next.and_then(|next| insns.next(next))
        } else if let Node::Insn(Insn::Jump { op: GOTO, target }) = node {
            insns.label_node(*target)
        } else {
            return Some(id);
        };
    }
    None
}

fn next_meaningful(insns: &InsnList, id: NodeId) -> Option<NodeId> {
    skip_until_meaningful(insns, insns.next(id), true)
}

fn is_unit_instance(node: &Node) -> bool {
    matches!(node, Node::Insn(Insn::Field { op: GETSTATIC, owner, name, .. })
        if owner == "kotlin/Unit" && name == "INSTANCE")
}

fn is_return_unit(insns: &InsnList, id: NodeId) -> bool {
    is_unit_instance(insns.node(id))
        && next_meaningful(insns, id).is_some_and(|next| {
            node_opcode(insns.node(next)) == Some(ARETURN) || is_pop_before_return_unit(insns, next)
        })
}

fn is_pop_before_return_unit(insns: &InsnList, id: NodeId) -> bool {
    node_opcode(insns.node(id)) == Some(POP)
        && next_meaningful(insns, id).is_some_and(|next| is_return_unit(insns, next))
}

/// `SuspensionPoint.isNoCall`: a point left with no call in it after inlining.
fn is_no_call(insns: &InsnList, point: &SuspensionPoint) -> bool {
    let mut cursor = insns.next(point.begin);
    while let Some(id) = cursor {
        if id == point.end {
            break;
        }
        let node = insns.node(id);
        match node_opcode(node) {
            Some(INVOKEVIRTUAL | INVOKESPECIAL | INVOKEINTERFACE | INVOKEDYNAMIC) => return false,
            Some(INVOKESTATIC)
                if !is_inline_marker(node, None) && !is_suspend_inline_marker(node) =>
            {
                return false
            }
            _ => {}
        }
        cursor = insns.next(id);
    }
    true
}

/// `SuspensionPoint.isUnitSuspendCall`: a `mark(11)` right after the opening marker.
fn is_unit_suspend_call(insns: &InsnList, point: &SuspensionPoint) -> bool {
    let after_marker = insns
        .next(point.begin)
        .and_then(|mark| insns.next(mark))
        .expect("an opening marker is followed by its call");
    skip_until_meaningful(insns, insns.next(after_marker), false)
        .is_some_and(|id| is_suspend_marker(insns, id, SuspendMarker::BeforeSuspendUnitCall))
}

const SAFE_OPCODES: [(u8, u8); 4] = [
    (NOP, NOP),
    (POP, SWAP),
    (IFEQ, GOTO),
    (CHECKCAST, CHECKCAST),
];

/// `allSuspensionPointsAreTailCalls`.
pub(crate) fn all_suspension_points_are_tail_calls(
    method: &EditableMethod,
    owner: &str,
    points: &[SuspensionPoint],
) -> Result<bool, CoroutineError> {
    let insns = &method.insns;
    let snapshot: MethodNode = method.snapshot();
    let mut interpreter = TcoInterpreter {
        ranges: points
            .iter()
            .map(|point| (insns.index_of(point.begin), insns.index_of(point.end)))
            .collect(),
    };
    // kotlinc runs ASM's `Analyzer` here; the fast analyzer computes the same stack tops, which
    // is all this check reads: a node that is not a jump target has one predecessor.
    let frames = analyze(&snapshot, owner, &mut interpreter).map_err(CoroutineError::Analysis)?;
    let graph = ControlFlowGraph::build(&snapshot, true);
    let ids = insns.ids();

    let invisible_in_debug = |index: usize| match &snapshot.nodes[index] {
        Node::Insn(Insn::Var { slot, .. }) => !method.method.local_variables.iter().any(|local| {
            local.slot == *slot
                && method.label_index(local.start) <= index
                && index <= method.label_index(local.end)
        }),
        _ => false,
    };
    let is_safe = |index: usize| {
        let node = &snapshot.nodes[index];
        let part_of_suspend_marker = is_suspend_inline_marker(node)
            || snapshot
                .nodes
                .get(index + 1)
                .is_some_and(is_suspend_inline_marker);
        !is_meaningful(node)
            || node_opcode(node).is_some_and(|op| {
                SAFE_OPCODES
                    .iter()
                    .any(|(low, high)| (*low..=*high).contains(&op))
            })
            || invisible_in_debug(index)
            || is_inline_marker(node, None)
            || part_of_suspend_marker
    };
    let top_is_from_suspension = |index: usize| match &frames[index] {
        Some(frame) => frame
            .stack
            .last()
            .is_none_or(|top| *top == TcoValue::FromSuspensionPoint),
        None => true,
    };
    let successors_are_safe_or_returns = |start: usize, unit_call: bool| {
        let mut visited = HashSet::from([start]);
        let mut stack = vec![start];
        while let Some(index) = stack.pop() {
            let node = &snapshot.nodes[index];
            if node_opcode(node) == Some(ARETURN)
                || (unit_call && is_pop_before_return_unit(insns, ids[index]))
            {
                if !top_is_from_suspension(index) {
                    return false;
                }
            } else if index != start && !is_safe(index) {
                return false;
            } else {
                for &next in graph.successors(index) {
                    if visited.insert(next) {
                        stack.push(next);
                    }
                }
            }
        }
        true
    };

    Ok(points.iter().all(|point| {
        let begin = insns.index_of(point.begin);
        method.method.try_catch_blocks.iter().all(|block| {
            begin < method.label_index(block.start) || method.label_index(block.end) <= begin
        }) && (is_no_call(insns, point)
            || successors_are_safe_or_returns(
                insns.index_of(point.end),
                is_unit_suspend_call(insns, point),
            ))
    }))
}

/// `addCoroutineSuspendedChecks`: return `COROUTINE_SUSPENDED` as soon as a tail call produces it,
/// unless the call's result is returned right away anyway.
pub(crate) fn add_coroutine_suspended_checks(
    method: &mut EditableMethod,
    points: &[SuspensionPoint],
) {
    for point in points {
        let returns_at_once = next_meaningful(&method.insns, point.end)
            .is_some_and(|next| node_opcode(method.insns.node(next)) == Some(ARETURN));
        if returns_at_once {
            continue;
        }
        let label = method.method.new_label();
        method.insns.insert_after(
            Some(point.end),
            vec![
                Node::Insn(Insn::Op(DUP)),
                coroutine_suspended(),
                Node::Insn(Insn::Jump {
                    op: IF_ACMPNE,
                    target: label,
                }),
                Node::Insn(Insn::Op(ARETURN)),
                Node::Label(label),
            ],
        );
    }
}
