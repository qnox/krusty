//! A suspension point: the instructions between a `mark(0)` and its `mark(1)`, with what the
//! transformation attaches to it (`SuspensionPoint` and `collectSuspensionPoints` in kotlinc's
//! `CoroutineTransformerMethodVisitor.kt`).

use std::collections::HashSet;

use super::super::analysis::ControlFlowGraph;
use super::super::insn_list::{EditableMethod, InsnList, NodeId};
use super::super::opcodes::*;
use super::markers::{is_suspend_marker, SuspendMarker};
use crate::jvm::bytecode_passes::analysis::node_opcode;
use crate::jvm::method_node::LabelId;

pub(crate) struct SuspensionPoint {
    /// The id push of the opening `mark(0)`.
    pub begin: NodeId,
    /// The closing `mark(1)` call.
    pub end: NodeId,
    /// Where the resumed state starts (`stateLabel`).
    pub state_label: LabelId,
    /// The label after which protected ranges split around this point resume
    /// (`tryCatchBlocksContinuationLabel`), set by `splitTryCatchBlocksContainingSuspensionPoint`.
    pub try_catch_continuation: Option<NodeId>,
    /// The inline-class unboxing between `mark(8)` and `mark(9)` right after the point, which the
    /// resume path repeats on the boxed result.
    pub unbox_inline_class: Vec<NodeId>,
}

impl SuspensionPoint {
    fn new(method: &mut EditableMethod, begin: NodeId, end: NodeId) -> Self {
        let unbox_inline_class = unbox_inline_class_instructions(&method.insns, end);
        SuspensionPoint {
            begin,
            end,
            state_label: method.method.new_label(),
            try_catch_continuation: None,
            unbox_inline_class,
        }
    }

    /// Whether `id` lies between `begin` and `end`, both included.
    pub(crate) fn contains(&self, insns: &InsnList, id: NodeId) -> bool {
        let index = insns.index_of(id);
        insns.index_of(self.begin) <= index && index <= insns.index_of(self.end)
    }

    /// `tryCatchBlockEndLabelAfterSuspensionCall`: the label split ranges end at, right after
    /// `end`.
    pub(crate) fn try_catch_end(&self, insns: &InsnList) -> NodeId {
        insns
            .next(self.end)
            .expect("a split suspension point is followed by its range-end label")
    }
}

/// Whether `id` lies inside any of `points`.
pub(crate) fn in_any(points: &[SuspensionPoint], insns: &InsnList, id: NodeId) -> bool {
    points.iter().any(|point| point.contains(insns, id))
}

fn unbox_inline_class_instructions(insns: &InsnList, end: NodeId) -> Vec<NodeId> {
    let Some(before) = insns.next(end).and_then(|next| insns.next(next)) else {
        return Vec::new();
    };
    if !is_suspend_marker(insns, before, SuspendMarker::BeforeUnboxInlineClass) {
        return Vec::new();
    }
    let mut cursor = insns.next(before);
    let mut after = None;
    while let Some(id) = cursor {
        if is_suspend_marker(insns, id, SuspendMarker::AfterUnboxInlineClass) {
            after = Some(id);
            break;
        }
        cursor = insns.next(id);
    }
    let after = after.expect("a before-unbox marker has its after-unbox marker");
    // Up to, not including, the id push of the after marker.
    let stop = insns
        .prev(insns.prev(after).expect("an id push"))
        .expect("a node");
    let mut result = Vec::new();
    let mut cursor = insns.next(before);
    while let Some(id) = cursor {
        if id == stop {
            break;
        }
        result.push(id);
        cursor = insns.next(id);
    }
    result
}

/// `collectSuspensionPoints`: each `mark(0)` with the `mark(1)` every path from it reaches. A point
/// nested in another, or one whose end is unreachable, is not a suspension point.
pub(crate) fn collect_suspension_points(method: &mut EditableMethod) -> Vec<SuspensionPoint> {
    let snapshot = method.snapshot();
    let graph = ControlFlowGraph::build(&snapshot, false);
    let ids = method.insns.ids();
    let is_before =
        |index: usize| is_suspend_marker(&method.insns, ids[index], SuspendMarker::BeforeSuspend);
    let is_after =
        |index: usize| is_suspend_marker(&method.insns, ids[index], SuspendMarker::AfterSuspend);

    let mut found = Vec::new();
    for start in 0..ids.len() {
        if !is_before(start) {
            continue;
        }
        let mut visited = HashSet::new();
        let mut ends = Vec::new();
        if collect_ends(
            start,
            &graph,
            &snapshot.nodes,
            &is_before,
            &is_after,
            &mut visited,
            &mut ends,
        ) {
            continue;
        }
        let Some(end) = ends.into_iter().find(|&end| is_after(end)) else {
            continue;
        };
        let begin = method
            .insns
            .prev(ids[start])
            .expect("a mark call follows its id push");
        found.push((begin, ids[end]));
    }
    found
        .into_iter()
        .map(|(begin, end)| SuspensionPoint::new(method, begin, end))
        .collect()
}

/// The depth-first search of `collectSuspensionPointEnds`, in kotlinc's visiting order; `true`
/// when another suspension point starts inside this one.
fn collect_ends(
    index: usize,
    graph: &ControlFlowGraph,
    nodes: &[crate::jvm::method_node::Node],
    is_before: &dyn Fn(usize) -> bool,
    is_after: &dyn Fn(usize) -> bool,
    visited: &mut HashSet<usize>,
    ends: &mut Vec<usize>,
) -> bool {
    if !visited.insert(index) {
        return false;
    }
    let op = node_opcode(&nodes[index]);
    if op == Some(ARETURN) || op == Some(ATHROW) || is_after(index) {
        if !ends.contains(&index) {
            ends.push(index);
        }
        return false;
    }
    for &successor in graph.successors(index) {
        if is_before(successor) {
            return true;
        }
        if collect_ends(successor, graph, nodes, is_before, is_after, visited, ends) {
            return true;
        }
    }
    false
}
