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

/// A local's value at a program point: which temporary stores it may hold. A dirty value names its
/// stores by an index into [`DirtySets`], which keeps a state a plain array to copy and compare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    Unknown,
    Store(usize),
    Dirty(usize),
}

/// The store sets dirty values name. A set never changes once added, so values share them freely.
#[derive(Default)]
struct DirtySets(Vec<BTreeSet<usize>>);

impl DirtySets {
    fn add(&mut self, stores: BTreeSet<usize>) -> Held {
        self.0.push(stores);
        Held::Dirty(self.0.len() - 1)
    }

    /// Whether `held` may hold `store`.
    fn holds(&self, held: Held, store: usize) -> bool {
        match held {
            Held::Unknown => false,
            Held::Store(own) => own == store,
            Held::Dirty(set) => self.0[set].contains(&store),
        }
    }

    /// Mark every store `held` may hold dirty.
    fn mark(&self, held: Held, dirty: &mut BTreeSet<usize>) {
        match held {
            Held::Unknown => {}
            Held::Store(store) => {
                dirty.insert(store);
            }
            Held::Dirty(set) => dirty.extend(self.0[set].iter().copied()),
        }
    }

    /// Join `other` into `held`; whether it changed.
    fn absorb(&mut self, held: &mut Held, other: Held) -> bool {
        if *held == other {
            return false;
        }
        // A dirty value that already holds every store of `other` absorbs it unchanged.
        let covered = matches!(held, Held::Dirty(_))
            && match other {
                Held::Unknown => true,
                Held::Store(store) => self.holds(*held, store),
                Held::Dirty(set) => self.0[set].iter().all(|&store| self.holds(*held, store)),
            };
        if covered {
            return false;
        }
        *held = match (*held, other) {
            (Held::Unknown, Held::Dirty(_)) => other,
            _ => {
                let mut stores = BTreeSet::new();
                self.mark(*held, &mut stores);
                self.mark(other, &mut stores);
                self.add(stores)
            }
        };
        true
    }
}

/// Join the state `base` with `overrides` applied into `current`; whether `current` changed.
fn join_into(
    sets: &mut DirtySets,
    current: &mut Option<Vec<Held>>,
    base: &[Held],
    overrides: &[(usize, Held)],
) -> bool {
    let Some(current) = current else {
        let mut incoming = base.to_vec();
        for &(slot, held) in overrides {
            incoming[slot] = held;
        }
        *current = Some(incoming);
        return true;
    };
    let mut changed = false;
    for (slot, (held, &base)) in current.iter_mut().zip(base).enumerate() {
        let incoming = overrides
            .iter()
            .find(|(overridden, _)| *overridden == slot)
            .map_or(base, |&(_, held)| held);
        changed |= sets.absorb(held, incoming);
    }
    changed
}

/// Every temporary store of `method` with the loads that read it, by instruction number, in store
/// order; `None` when the body is outside what the analysis models.
pub(super) fn temporaries(method: &MethodNode) -> Option<BTreeMap<usize, Vec<usize>>> {
    analyze(method, &mut || {})
}

/// [`temporaries`], calling `on_step` for every instruction step of the fixpoint.
pub(super) fn analyze(
    method: &MethodNode,
    on_step: &mut dyn FnMut(),
) -> Option<BTreeMap<usize, Vec<usize>>> {
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
    // An instruction is stepped again only when its incoming state changed since its last step.
    // Every side effect below grows with the state, so the fixpoint matches stepping every
    // instruction on every round.
    let mut pending = vec![false; n + 1];
    pending[0] = true;
    let mut sets = DirtySets::default();
    let mut loads: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut dirty: BTreeSet<usize> = BTreeSet::new();
    let order = graph.reverse_post_order();
    loop {
        let mut stepped = false;
        for &index in &order {
            if index >= n || !std::mem::take(&mut pending[index]) {
                continue;
            }
            let Some(state) = before[index].take() else {
                continue;
            };
            stepped = true;
            on_step();
            if let Some(slots) = named_starts.get(&index) {
                for &slot in slots {
                    if let Some(held) = state.get(usize::from(slot)) {
                        sets.mark(*held, &mut dirty);
                    }
                }
            }
            // The state after the instruction is the state before it with these slots replaced.
            let mut overrides: Vec<(usize, Held)> = Vec::new();
            match var_op(graph.insn(index)) {
                Some(VarOp::Load(_, slot)) => match &state[usize::from(slot)] {
                    Held::Store(store) => {
                        loads.entry(*store).or_default().insert(index);
                    }
                    Held::Dirty(_) => sets.mark(state[usize::from(slot)], &mut dirty),
                    Held::Unknown => {}
                },
                Some(VarOp::Store(kind, slot)) => {
                    let held = if candidate[index] {
                        Held::Store(index)
                    } else {
                        Held::Unknown
                    };
                    overrides.push((usize::from(slot), held));
                    if kind.words() == 2 {
                        overrides.push((usize::from(slot) + 1, Held::Unknown));
                    }
                }
                Some(VarOp::Iinc(slot)) => {
                    sets.mark(state[usize::from(slot)], &mut dirty);
                    overrides.push((usize::from(slot), Held::Unknown));
                }
                None => {}
            }
            // An edge back to this instruction joins into its own state once the step is done.
            let mut into_self: Vec<Vec<Held>> = Vec::new();
            let mut flow = |to: usize, overrides: &[(usize, Held)]| {
                if to > n {
                    return;
                }
                if to == index {
                    let mut incoming = state.clone();
                    for &(slot, held) in overrides {
                        incoming[slot] = held;
                    }
                    into_self.push(incoming);
                } else if join_into(&mut sets, &mut before[to], &state, overrides) {
                    pending[to] = true;
                }
            };
            for &to in graph.normal_successors(index) {
                flow(to, &overrides);
            }
            for &handler in graph.exceptional_successors(index) {
                flow(handler, &[]);
                flow(handler, &overrides);
            }
            let slot = &mut before[index];
            *slot = Some(state);
            for incoming in into_self {
                if join_into(&mut sets, slot, &incoming, &[]) {
                    pending[index] = true;
                }
            }
        }
        if !stepped {
            break;
        }
    }
    let temporaries: BTreeMap<usize, Vec<usize>> = (0..n)
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
        .collect();
    Some(temporaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_join_changes_a_value_exactly_when_the_stores_it_may_hold_change() {
        let mut sets = DirtySets::default();
        let mut held = Held::Store(1);
        assert!(!sets.absorb(&mut held, Held::Store(1)));
        // A lone store that meets its own dirty set becomes dirty: its loads no longer read it alone.
        let own = sets.add(BTreeSet::from([1]));
        assert!(sets.absorb(&mut held, own));
        assert!(matches!(held, Held::Dirty(_)));
        assert!(!sets.absorb(&mut held, Held::Unknown));
        assert!(!sets.absorb(&mut held, Held::Store(1)));
        assert!(sets.absorb(&mut held, Held::Store(2)));
        let both = sets.add(BTreeSet::from([2, 1]));
        assert!(
            !sets.absorb(&mut held, both),
            "an equal set under another index"
        );
        let mut marked = BTreeSet::new();
        sets.mark(held, &mut marked);
        assert_eq!(marked, BTreeSet::from([1, 2]));

        let mut unknown = Held::Unknown;
        assert!(sets.absorb(&mut unknown, Held::Store(3)));
        let mut marked = BTreeSet::new();
        sets.mark(unknown, &mut marked);
        assert_eq!(marked, BTreeSet::from([3]));
    }
}
