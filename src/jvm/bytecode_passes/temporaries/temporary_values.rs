//! kotlinc's `TemporaryValsAnalyzer`: which stores are temporaries, and which loads read each.
//!
//! A value is a temporary when no local-variable range covers or begins after its store, it is not
//! an exception handler's catch store, and every load it reaches reads only it. A store whose value
//! reaches a merge with another value, an `iinc`, or the start of a named local is dirty and not a
//! temporary.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::super::instruction_graph::InstructionGraph;
use super::null_check_folds::analysis_within_limit;
use super::shapes::{is_terminator, var_op, Kind, VarOp};
use crate::jvm::method_node::{LabelId, MethodNode, Node};

/// A local's value at a program point: which temporary stores it may hold.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Held {
    Unknown,
    Store(usize),
    Dirty(BTreeSet<usize>),
}

impl Held {
    fn stores(&self) -> BTreeSet<usize> {
        match self {
            Held::Unknown => BTreeSet::new(),
            Held::Store(store) => BTreeSet::from([*store]),
            Held::Dirty(stores) => stores.clone(),
        }
    }

    fn join(&self, other: &Held) -> Held {
        if self == other {
            return self.clone();
        }
        let mut stores = self.stores();
        stores.extend(other.stores());
        if stores.is_empty() {
            Held::Unknown
        } else {
            Held::Dirty(stores)
        }
    }
}

/// Every temporary store of `method` with the loads that read it, by instruction number, in store
/// order; `None` when the body is outside what the analysis models.
pub(super) fn temporaries(method: &MethodNode) -> Option<BTreeMap<usize, Vec<usize>>> {
    let graph = InstructionGraph::build(method)?;
    let n = graph.len();
    let mut kept: HashSet<LabelId> = HashSet::new();
    for node in &method.nodes {
        match node {
            Node::Insn(insn) => kept.extend(insn.jump_targets()),
            Node::Line { start, .. } => {
                kept.insert(*start);
            }
            Node::Label(_) => {}
        }
    }
    for block in &method.try_catch_blocks {
        kept.extend([block.start, block.end, block.handler]);
    }
    let mut slot_count = 0usize;
    for index in 0..n {
        let end = match var_op(graph.insn(index)) {
            Some(VarOp::Load(kind, slot) | VarOp::Store(kind, slot)) => {
                usize::from(slot) + usize::from(kind.words())
            }
            Some(VarOp::Iinc(slot)) => usize::from(slot) + 1,
            None => continue,
        };
        if end > usize::from(u16::MAX) {
            return None;
        }
        slot_count = slot_count.max(end);
    }
    let mut candidate: Vec<bool> = (0..n)
        .map(|index| matches!(var_op(graph.insn(index)), Some(VarOp::Store(..))))
        .collect();
    // Named locals: `(start, end, slot)`, end exclusive; one starting after the last instruction
    // covers none.
    let named: Vec<(usize, usize, u16)> = method
        .local_variables
        .iter()
        .map(|local| {
            (
                graph.at_label(local.start),
                graph.at_label(local.end),
                local.slot,
            )
        })
        .filter(|&(start, _, _)| start < n)
        .collect();
    for local in &method.local_variables {
        kept.extend([local.start, local.end]);
    }
    let labelled = |index: usize| {
        graph
            .labels_before(index)
            .any(|label| kept.contains(&label))
    };
    let stores_to = |index: usize, slot: u16| matches!(var_op(graph.insn(index)), Some(VarOp::Store(_, s)) if s == slot);
    // A named local's initializing store, and every store inside its range, belongs to the named
    // variable.
    for &(start, end, slot) in &named {
        for (index, is_candidate) in candidate.iter_mut().enumerate().take(end).skip(start) {
            if stores_to(index, slot) {
                *is_candidate = false;
            }
        }
        // The range-start label may follow checks or other instructions after the initializer.
        // Walk backward to the first store this label can observe, stopping at another kept label
        // or at an instruction that cannot fall through. This is the declaration identity carried
        // by the local's range; adjacency alone is not enough to find its initializer.
        let mut cursor = start;
        while let Some(index) = cursor.checked_sub(1) {
            if stores_to(index, slot) {
                candidate[index] = false;
                break;
            }
            if labelled(index) || is_terminator(graph.insn(index)) {
                break;
            }
            cursor = index;
        }
    }
    // A handler's catch store, and the stores right before a protected range, are not temporaries.
    // The upstream pass also recognizes a catch store hidden behind reified-operation marker calls.
    // Those calls have no typed representation here, so decline the whole rewrite instead of
    // guessing which later `astore` owns the exception.
    for block in &method.try_catch_blocks {
        let catch_store = graph.at_label(block.handler);
        if catch_store >= n
            || !matches!(
                var_op(graph.insn(catch_store)),
                Some(VarOp::Store(Kind::Reference, _))
            )
        {
            return None;
        }
        candidate[catch_store] = false;
        let mut index = graph.at_label(block.start);
        while let Some(previous) = index.checked_sub(1) {
            if !matches!(var_op(graph.insn(previous)), Some(VarOp::Store(..))) {
                break;
            }
            candidate[previous] = false;
            index = previous;
        }
    }
    let mut named_starts: BTreeMap<usize, Vec<u16>> = BTreeMap::new();
    for &(start, _, slot) in &named {
        named_starts.entry(start).or_default().push(slot);
    }
    let width = slot_count.max(1);
    let candidate_count = candidate
        .iter()
        .filter(|&&is_candidate| is_candidate)
        .count();
    if !analysis_within_limit(n, width, candidate_count) {
        return None;
    }
    let mut before: Vec<Option<Vec<Held>>> = vec![None; n + 1];
    before[0] = Some(vec![Held::Unknown; width]);
    let mut loads: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut dirty: BTreeSet<usize> = BTreeSet::new();
    let order = graph.reverse_post_order();
    loop {
        let mut changed = false;
        for &index in &order {
            if index >= n {
                continue;
            }
            let Some(state) = before[index].clone() else {
                continue;
            };
            if let Some(slots) = named_starts.get(&index) {
                for &slot in slots {
                    if let Some(held) = state.get(usize::from(slot)) {
                        dirty.extend(held.stores());
                    }
                }
            }
            let mut after = state.clone();
            match var_op(graph.insn(index)) {
                Some(VarOp::Load(_, slot)) => match &state[usize::from(slot)] {
                    Held::Store(store) => {
                        loads.entry(*store).or_default().insert(index);
                    }
                    Held::Dirty(stores) => dirty.extend(stores.iter().copied()),
                    Held::Unknown => {}
                },
                Some(VarOp::Store(kind, slot)) => {
                    after[usize::from(slot)] = if candidate[index] {
                        Held::Store(index)
                    } else {
                        Held::Unknown
                    };
                    if kind.words() == 2 {
                        after[usize::from(slot) + 1] = Held::Unknown;
                    }
                }
                Some(VarOp::Iinc(slot)) => {
                    dirty.extend(state[usize::from(slot)].stores());
                    after[usize::from(slot)] = Held::Unknown;
                }
                None => {}
            }
            let mut propagate = |to: usize, incoming: &[Held]| {
                if to > n {
                    return;
                }
                match &mut before[to] {
                    Some(current) => {
                        for (slot, held) in current.iter_mut().enumerate() {
                            let joined = held.join(&incoming[slot]);
                            if joined != *held {
                                *held = joined;
                                changed = true;
                            }
                        }
                    }
                    slot @ None => {
                        *slot = Some(incoming.to_vec());
                        changed = true;
                    }
                }
            };
            for &to in graph.normal_successors(index) {
                propagate(to, &after);
            }
            for &handler in graph.exceptional_successors(index) {
                propagate(handler, &state);
                propagate(handler, &after);
            }
        }
        if !changed {
            break;
        }
    }
    Some(
        (0..n)
            .filter(|&index| candidate[index] && !dirty.contains(&index))
            .filter(|&index| before[index].is_some())
            .map(|index| {
                (
                    index,
                    loads
                        .get(&index)
                        .map(|found| found.iter().copied().collect())
                        .unwrap_or_default(),
                )
            })
            .collect(),
    )
}
