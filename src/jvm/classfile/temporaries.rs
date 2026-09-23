//! kotlinc's temporary-variable elimination over a finished JVM method body.
//!
//! kotlinc's JVM backend writes a temporary (an unnamed local with no `LocalVariableTable` entry) as
//! an ordinary `store`/`load` pair, then a bytecode pass rewrites the pairs whose value can stay on
//! the operand stack (`TemporaryVariablesEliminationTransformer`). This module implements the
//! local store/load rules whose safety can be established from the finished class-file tables:
//!
//! - `xload; pop` (`pop2` for a two-word value) is removed;
//! - a temporary never loaded is stored as a `pop`/`pop2` instead;
//! - a temporary loaded once, with nothing that executes between its store and that load, is
//!   removed together with the load;
//! - `astore t; aload y; aload t` becomes `aload y; swap`, and `astore t; getstatic f; aload t` (a
//!   one-word `f`) becomes `getstatic f; swap`;
//! - `astore t; aload t; ldc "…"; invokestatic Intrinsics.checkNotNullExpressionValue; aload t`
//!   becomes `dup; ldc "…"; invokestatic …`.
//!
//! A value is a temporary when no `LocalVariableTable` range covers or begins after its store,
//! it is not an exception handler's catch store, and every load it reaches reads only it. Patterns
//! match raw adjacency, as kotlinc's do: a line number or a branch target between the instructions
//! defeats them. Safe-call-chain reshaping and unused-LVT cleanup are separate kotlinc optimizations
//! and are deliberately not approximated here; a body containing a safe-call input shape is left
//! unchanged, while a rewrite that would create an empty debug-local range is rejected by the
//! class-file boundary.
//!
//! This module decides the rewrite on the original instruction indices; the class-file writer
//! applies it and keeps every offset-keyed table in step.

use std::collections::{BTreeMap, BTreeSet};

use super::bytecode_analysis::{ControlGraph, Handler};
use crate::jvm::inline::{BranchTarget, Insn};

/// Where an instruction of the rewritten body sits relative to the original instructions. A label or
/// line number that stood at original instruction `k` stays in front of `k`'s group:
/// `Before(k)*`, `Original(k)`, `After(k)*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    Original(usize),
    Before(usize),
    After(usize),
}

impl Placement {
    pub(crate) fn group(self) -> usize {
        match self {
            Placement::Original(k) | Placement::Before(k) | Placement::After(k) => k,
        }
    }
}

/// What the rewrite needs to know about the body beyond its instructions.
pub(crate) struct Body<'a> {
    pub insns: &'a [Insn],
    pub handlers: &'a [Handler],
    /// `true` at an original index some branch, switch or handler reaches, or where a protected
    /// range begins or ends — kotlinc's labels with non-trivial predecessors. Length `insns.len() + 1`.
    pub arrivals: &'a [bool],
    /// `true` at an original index that begins a `LineNumberTable` entry, or where a named local's
    /// range begins or ends — the other labels kotlinc keeps in its instruction list.
    pub marks: &'a [bool],
    /// Named locals: `(start index, end index, slot)`, end exclusive.
    pub named: &'a [(usize, usize, u16)],
    /// Whether a `getstatic` operand's field is one JVM word.
    pub one_word_static: &'a dyn Fn(u16) -> bool,
    /// Whether an `ldc`/`ldc_w` operand is a `String` constant.
    pub string_constant: &'a dyn Fn(u16) -> bool,
    /// Whether an `invokestatic` operand is `Intrinsics.checkNotNullExpressionValue` or the older
    /// `checkExpressionValueIsNotNull`.
    pub expression_null_check: &'a dyn Fn(u16) -> bool,
}

/// The rewritten body, and every eliminated temporary as `(original store index, slot)`: its store
/// was removed or turned into a `pop`, so the slot no longer holds its value anywhere it reached.
pub(crate) struct Rewrite {
    pub nodes: Vec<(Insn, Placement)>,
    pub eliminated: Vec<(usize, u16)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Int,
    Long,
    Float,
    Double,
    Reference,
}

impl Kind {
    fn from_index(index: u8) -> Kind {
        match index {
            0 => Kind::Int,
            1 => Kind::Long,
            2 => Kind::Float,
            3 => Kind::Double,
            _ => Kind::Reference,
        }
    }

    fn words(self) -> u16 {
        match self {
            Kind::Long | Kind::Double => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VarOp {
    Load(Kind, u16),
    Store(Kind, u16),
    Iinc(u16),
}

fn var_op(insn: &Insn) -> Option<VarOp> {
    let Insn::Plain { op, operands } = insn else {
        return None;
    };
    let op = *op;
    Some(match op {
        0x15..=0x19 => VarOp::Load(Kind::from_index(op - 0x15), u16::from(*operands.first()?)),
        0x1a..=0x2d => {
            let n = op - 0x1a;
            VarOp::Load(Kind::from_index(n / 4), u16::from(n % 4))
        }
        0x36..=0x3a => VarOp::Store(Kind::from_index(op - 0x36), u16::from(*operands.first()?)),
        0x3b..=0x4e => {
            let n = op - 0x3b;
            VarOp::Store(Kind::from_index(n / 4), u16::from(n % 4))
        }
        0x84 => VarOp::Iinc(u16::from(*operands.first()?)),
        0xc4 => {
            let inner = *operands.first()?;
            let slot = u16::from_be_bytes([*operands.get(1)?, *operands.get(2)?]);
            match inner {
                0x15..=0x19 => VarOp::Load(Kind::from_index(inner - 0x15), slot),
                0x36..=0x3a => VarOp::Store(Kind::from_index(inner - 0x36), slot),
                0x84 => VarOp::Iinc(slot),
                _ => return None,
            }
        }
        _ => return None,
    })
}

fn opcode(insn: &Insn) -> Option<u8> {
    match insn {
        Insn::Plain { op, .. } => Some(*op),
        _ => None,
    }
}

fn u2_operand(insn: &Insn) -> Option<u16> {
    let Insn::Plain { operands, .. } = insn else {
        return None;
    };
    Some(u16::from_be_bytes([*operands.first()?, *operands.get(1)?]))
}

fn plain(op: u8) -> Insn {
    Insn::Plain {
        op,
        operands: Vec::new(),
    }
}

const POP: u8 = 0x57;
const POP2: u8 = 0x58;
const DUP: u8 = 0x59;
const SWAP: u8 = 0x5f;
const NOP: u8 = 0x00;
const GETSTATIC: u8 = 0xb2;
const LDC: u8 = 0x12;
const LDC_W: u8 = 0x13;
const INVOKESTATIC: u8 = 0xb8;
const IFNULL: u8 = 0xc6;
const IFNONNULL: u8 = 0xc7;

/// Same 50 MiB-by-method complexity ceiling as kotlinc's optimizer. The product is deliberately
/// conservative for this representation: a slot/store analysis cell is larger than one byte.
const ANALYSIS_COMPLEXITY_LIMIT: usize = 50 * 1024 * 1024;

fn analysis_within_limit(instructions: usize, slots: usize, candidates: usize) -> bool {
    instructions
        .checked_mul(slots)
        .and_then(|cells| cells.checked_mul(candidates.max(1)))
        .is_some_and(|cells| cells <= ANALYSIS_COMPLEXITY_LIMIT)
}

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

/// kotlinc's `TemporaryValsAnalyzer`: every temporary store with the loads that read it, in store
/// order. A store whose value reaches a merge with another value, an `iinc`, or the start of a named
/// local is dirty and not returned.
fn temporaries(body: &Body, removed: &BTreeSet<usize>) -> Option<BTreeMap<usize, Vec<usize>>> {
    let insns = body.insns;
    let graph = ControlGraph::build(insns, body.handlers)?;
    let mut slot_count = 0usize;
    for insn in insns {
        if let Some(VarOp::Load(kind, slot) | VarOp::Store(kind, slot)) = var_op(insn) {
            let end = usize::from(slot).checked_add(usize::from(kind.words()))?;
            if end > usize::from(u16::MAX) {
                return None;
            }
            slot_count = slot_count.max(end);
        } else if let Some(VarOp::Iinc(slot)) = var_op(insn) {
            let end = usize::from(slot).checked_add(1)?;
            if end > usize::from(u16::MAX) {
                return None;
            }
            slot_count = slot_count.max(end);
        }
    }
    let mut candidate = vec![false; insns.len()];
    for (index, insn) in insns.iter().enumerate() {
        candidate[index] = matches!(var_op(insn), Some(VarOp::Store(..)));
    }
    // A named local's initializing store, and every store inside its range, belongs to the named
    // variable.
    for &(start, end, slot) in body.named {
        for (index, insn) in insns.iter().enumerate().take(end).skip(start) {
            if matches!(var_op(insn), Some(VarOp::Store(_, s)) if s == slot) {
                candidate[index] = false;
            }
        }
        // The range-start label may follow checks or other instructions after the initializer.
        // Walk backward to the first store this label can observe, stopping at another retained
        // label or at an instruction that cannot fall through. This is the declaration identity
        // carried by the LVT range; adjacency alone is not enough to find its initializer.
        let mut cursor = start;
        while let Some(index) = cursor.checked_sub(1) {
            if matches!(var_op(&insns[index]), Some(VarOp::Store(_, s)) if s == slot) {
                candidate[index] = false;
                break;
            }
            let stops = body.arrivals[index]
                || body.marks[index]
                || matches!(
                    &insns[index],
                    Insn::Branch { op: 0xa7, .. }
                        | Insn::BranchW { op: 0xc8, .. }
                        | Insn::Plain {
                            op: 0xac..=0xb1 | 0xbf,
                            ..
                        }
                );
            if stops {
                break;
            }
            cursor = index;
        }
    }
    // A handler's catch store, and the stores right before a protected range, are not temporaries.
    // The upstream pass also recognizes a catch store hidden behind reified-operation marker calls.
    // Those calls have no typed representation here, so decline the whole rewrite instead of
    // guessing which later ASTORE owns the exception.
    for handler in body.handlers {
        let catch_store = handler.handler;
        if !matches!(
            insns.get(catch_store).and_then(var_op),
            Some(VarOp::Store(Kind::Reference, _))
        ) {
            return None;
        }
        candidate[catch_store] = false;
        let mut index = handler.start;
        while let Some(previous) = index.checked_sub(1) {
            if !matches!(var_op(&insns[previous]), Some(VarOp::Store(..))) {
                break;
            }
            candidate[previous] = false;
            index = previous;
        }
    }
    let named_starts: BTreeMap<usize, Vec<u16>> =
        body.named
            .iter()
            .fold(BTreeMap::new(), |mut starts, &(start, _, slot)| {
                starts.entry(start).or_insert_with(Vec::new).push(slot);
                starts
            });
    let width = slot_count.max(1);
    let candidate_count = candidate
        .iter()
        .filter(|&&is_candidate| is_candidate)
        .count();
    if !analysis_within_limit(insns.len(), width, candidate_count) {
        return None;
    }
    let mut before: Vec<Option<Vec<Held>>> = vec![None; insns.len() + 1];
    before[0] = Some(vec![Held::Unknown; width]);
    let mut loads: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut dirty: BTreeSet<usize> = BTreeSet::new();
    let order = graph.reverse_post_order();
    loop {
        let mut changed = false;
        for &index in &order {
            if index >= insns.len() {
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
            if !removed.contains(&index) {
                match var_op(&insns[index]) {
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
            }
            let mut propagate = |to: usize, incoming: &[Held]| {
                if to > insns.len() {
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
        insns
            .iter()
            .enumerate()
            .filter(|&(index, _)| candidate[index] && !dirty.contains(&index))
            .filter(|&(index, _)| before[index].is_some())
            .map(|(index, _)| {
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

/// The working instruction list: original instructions plus insertions, in order.
struct Working<'a> {
    body: &'a Body<'a>,
    nodes: Vec<(Insn, Placement)>,
}

impl Working<'_> {
    fn position(&self, original: usize) -> Option<usize> {
        self.nodes
            .iter()
            .position(|(_, placement)| *placement == Placement::Original(original))
    }

    /// Whether a label kotlinc keeps stands in front of node `at`: the first node of an original
    /// group whose index carries an arrival or a mark.
    fn labelled(&self, at: usize) -> bool {
        let group = self.nodes[at].1.group();
        let first = at == 0 || self.nodes[at - 1].1.group() != group;
        first && (self.body.arrivals[group] || self.body.marks[group])
    }

    /// Whether a label with non-trivial predecessors stands in front of node `at`.
    fn arrived(&self, at: usize) -> bool {
        let group = self.nodes[at].1.group();
        let first = at == 0 || self.nodes[at - 1].1.group() != group;
        first && self.body.arrivals[group]
    }

    /// `nodes[at..at + ops.len()]` match `ops` with nothing kotlinc keeps between them.
    fn raw_sequence(&self, at: usize, len: usize) -> bool {
        at + len <= self.nodes.len() && (at + 1..at + len).all(|next| !self.labelled(next))
    }

    fn remove(&mut self, positions: &mut [usize]) {
        positions.sort_unstable();
        for &at in positions.iter().rev() {
            self.nodes.remove(at);
        }
    }
}

/// Decide kotlinc's rewrite of `body`, or `None` when nothing applies or the body is outside what
/// the analysis models.
pub(crate) fn eliminate(body: &Body) -> Option<Rewrite> {
    let insns = body.insns;
    if insns.iter().any(|insn| {
        matches!(
            insn,
            Insn::Branch {
                target: BranchTarget::External(_),
                ..
            } | Insn::BranchW {
                target: BranchTarget::External(_),
                ..
            }
        )
    }) {
        return None;
    }
    let mut working = Working {
        body,
        nodes: insns
            .iter()
            .enumerate()
            .map(|(index, insn)| (insn.clone(), Placement::Original(index)))
            .collect(),
    };
    let mut eliminated = Vec::new();
    // `xload; pop` — the value loaded only to be discarded.
    let mut trivially_removed = BTreeSet::new();
    let mut at = 0;
    while at + 1 < working.nodes.len() {
        let pair = match (
            var_op(&working.nodes[at].0),
            opcode(&working.nodes[at + 1].0),
        ) {
            (Some(VarOp::Load(kind, _)), Some(POP)) => kind.words() == 1,
            (Some(VarOp::Load(kind, _)), Some(POP2)) => kind.words() == 2,
            _ => false,
        };
        if pair && working.raw_sequence(at, 2) {
            if let Placement::Original(load) = working.nodes[at].1 {
                trivially_removed.insert(load);
                if body.handlers.iter().any(|handler| handler.start == load) {
                    // kotlinc first replaces the pair by `nop`, then retains it at a protected
                    // region's start so the exception range cannot collapse.
                    working.nodes[at].0 = plain(NOP);
                    working.remove(&mut [at + 1]);
                    at += 1;
                    continue;
                }
            }
            working.remove(&mut [at, at + 1]);
        } else {
            at += 1;
        }
    }
    // Match kotlinc's second trivial-cleanup sweep. In the class file, every label that still
    // matters is represented by an arrival or debug-table mark at an instruction group. A NOP is
    // adjacent to a meaningful instruction on a side exactly when no such label separates them.
    // A protected-range start is the sole exception: it retains its NOP to keep the range nonempty.
    let mut removed_nop = false;
    let mut at = 0;
    while at < working.nodes.len() {
        if opcode(&working.nodes[at].0) != Some(NOP) {
            at += 1;
            continue;
        }
        let group = working.nodes[at].1.group();
        let protected_start = body.handlers.iter().any(|handler| handler.start == group);
        let meaningful_before = at > 0 && !working.labelled(at);
        let meaningful_after = at + 1 < working.nodes.len() && !working.labelled(at + 1);
        if !protected_start && (meaningful_before || meaningful_after) {
            working.remove(&mut [at]);
            removed_nop = true;
        } else {
            at += 1;
        }
    }
    // Safe-call rewriting runs after trivial NOP cleanup upstream. Detect that exact raw shape in
    // the cleaned working list and preserve the original method rather than applying only half of
    // the coupled transformation.
    if (0..working.nodes.len().saturating_sub(1)).any(|at| {
        working.raw_sequence(at, 2)
            && matches!(
                var_op(&working.nodes[at].0),
                Some(VarOp::Load(Kind::Reference, _))
            )
            && matches!(
                working.nodes[at + 1].0,
                Insn::Branch {
                    op: IFNULL | IFNONNULL,
                    ..
                }
            )
    }) {
        return None;
    }
    let changed_trivially = !trivially_removed.is_empty() || removed_nop;
    let temporaries = temporaries(body, &trivially_removed)?;
    let mut changed = changed_trivially;
    for (store, loads) in temporaries {
        let Some(store_at) = working.position(store) else {
            continue;
        };
        let Some(VarOp::Store(kind, slot)) = var_op(&working.nodes[store_at].0) else {
            continue;
        };
        let load_at: Vec<usize> = loads
            .iter()
            .filter_map(|&load| working.position(load))
            .collect();
        if load_at.len() != loads.len() {
            continue;
        }
        match load_at.as_slice() {
            [] => {
                working.nodes[store_at].0 = plain(if kind.words() == 2 { POP2 } else { POP });
                eliminated.push((store, slot));
                changed = true;
            }
            [load_at] => {
                let load_at = *load_at;
                // Nothing that executes between the store and its load: only `nop`s, and no label
                // any control flow reaches.
                let adjacent = load_at > store_at
                    && (store_at + 1..load_at)
                        .all(|between| opcode(&working.nodes[between].0) == Some(NOP))
                    && (store_at + 1..=load_at).all(|next| !working.arrived(next));
                if adjacent {
                    working.remove(&mut [store_at, load_at]);
                    eliminated.push((store, slot));
                    changed = true;
                    continue;
                }
                if kind != Kind::Reference
                    || load_at != store_at + 2
                    || !working.raw_sequence(store_at, 3)
                {
                    continue;
                }
                let middle = &working.nodes[store_at + 1].0;
                let swappable = matches!(var_op(middle), Some(VarOp::Load(Kind::Reference, _)))
                    || (opcode(middle) == Some(GETSTATIC)
                        && u2_operand(middle).is_some_and(|field| (body.one_word_static)(field)));
                if swappable {
                    let middle_group = working.nodes[store_at + 1].1;
                    let swap_placement = match middle_group {
                        Placement::Before(k) => Placement::Before(k),
                        Placement::Original(k) | Placement::After(k) => Placement::After(k),
                    };
                    working
                        .nodes
                        .insert(store_at + 2, (plain(SWAP), swap_placement));
                    // The load moved one position to the right past the inserted `swap`.
                    working.remove(&mut [store_at, load_at + 1]);
                    eliminated.push((store, slot));
                    changed = true;
                }
            }
            [first, last] => {
                let (first, last) = (*first, *last);
                if kind != Kind::Reference
                    || first != store_at + 1
                    || last != store_at + 4
                    || !working.raw_sequence(store_at, 5)
                {
                    continue;
                }
                let ldc = &working.nodes[store_at + 2].0;
                let is_string = match ldc {
                    Insn::Plain { op: LDC, operands } => operands
                        .first()
                        .is_some_and(|&index| (body.string_constant)(u16::from(index))),
                    Insn::Plain { op: LDC_W, .. } => {
                        u2_operand(ldc).is_some_and(|index| (body.string_constant)(index))
                    }
                    _ => false,
                };
                let check = &working.nodes[store_at + 3].0;
                let is_check = opcode(check) == Some(INVOKESTATIC)
                    && u2_operand(check).is_some_and(|method| (body.expression_null_check)(method));
                if !is_string || !is_check {
                    continue;
                }
                let ldc_group = working.nodes[store_at + 2].1;
                let dup_placement = match ldc_group {
                    Placement::Original(k) | Placement::Before(k) => Placement::Before(k),
                    Placement::After(k) => Placement::After(k),
                };
                working
                    .nodes
                    .insert(store_at + 2, (plain(DUP), dup_placement));
                // `store`, the first load, and the last load (shifted by the inserted `dup`).
                working.remove(&mut [store_at, store_at + 1, last + 1]);
                eliminated.push((store, slot));
                changed = true;
            }
            _ => {}
        }
    }
    changed.then_some(Rewrite {
        nodes: working.nodes,
        eliminated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(op: u8) -> Insn {
        plain(op)
    }

    fn with(op: u8, operands: &[u8]) -> Insn {
        Insn::Plain {
            op,
            operands: operands.to_vec(),
        }
    }

    fn rewrite_with(
        insns: &[Insn],
        arrivals: &[usize],
        marks: &[usize],
        named: &[(usize, usize, u16)],
        handlers: &[Handler],
    ) -> Option<Vec<Insn>> {
        let mut arrival = vec![false; insns.len() + 1];
        for &index in arrivals {
            arrival[index] = true;
        }
        let mut mark = vec![false; insns.len() + 1];
        for &index in marks {
            mark[index] = true;
        }
        let body = Body {
            insns,
            handlers,
            arrivals: &arrival,
            marks: &mark,
            named,
            one_word_static: &|field| field == 1,
            string_constant: &|index| index == 7,
            expression_null_check: &|method| method == 9,
        };
        eliminate(&body).map(|rewrite| rewrite.nodes.into_iter().map(|(insn, _)| insn).collect())
    }

    fn rewrite(insns: &[Insn], arrivals: &[usize], marks: &[usize]) -> Option<Vec<Insn>> {
        rewrite_with(insns, arrivals, marks, &[], &[])
    }

    const ALOAD_0: u8 = 0x2a;
    const ALOAD_1: u8 = 0x2b;
    const ASTORE_1: u8 = 0x4c;
    const ARETURN: u8 = 0xb0;

    #[test]
    fn a_store_followed_by_its_only_load_leaves_the_value_on_the_stack() {
        let insns = [op(ALOAD_0), op(ASTORE_1), op(ALOAD_1), op(ARETURN)];
        assert_eq!(
            rewrite(&insns, &[], &[]),
            Some(vec![op(ALOAD_0), op(ARETURN)])
        );
    }

    #[test]
    fn a_line_mark_between_store_and_load_does_not_intervene() {
        let insns = [op(ALOAD_0), op(ASTORE_1), op(ALOAD_1), op(ARETURN)];
        assert_eq!(
            rewrite(&insns, &[], &[2]),
            Some(vec![op(ALOAD_0), op(ARETURN)])
        );
    }

    #[test]
    fn a_named_local_initializer_is_found_past_non_intervening_instructions() {
        let insns = [
            op(ALOAD_0),
            op(ASTORE_1),
            op(0x03), // iconst_0
            op(POP),
            op(0xb1),
        ];
        assert_eq!(rewrite_with(&insns, &[], &[3], &[(3, 5, 1)], &[]), None);
    }

    #[test]
    fn a_branch_target_between_store_and_load_intervenes() {
        let insns = [op(ALOAD_0), op(ASTORE_1), op(ALOAD_1), op(ARETURN)];
        assert_eq!(rewrite(&insns, &[2], &[]), None);
    }

    #[test]
    fn a_value_loaded_after_another_load_is_swapped_into_place() {
        // astore_1; aload_0; aload_1; invokevirtual → aload_0; swap; invokevirtual
        let call = with(0xb6, &[0, 3]);
        let insns = [
            op(ALOAD_0),
            op(ASTORE_1),
            op(ALOAD_0),
            op(ALOAD_1),
            call.clone(),
            op(ARETURN),
        ];
        assert_eq!(
            rewrite(&insns, &[], &[]),
            Some(vec![op(ALOAD_0), op(ALOAD_0), op(SWAP), call, op(ARETURN)])
        );
    }

    #[test]
    fn a_one_word_static_before_the_load_is_swapped_into_place() {
        let field = with(GETSTATIC, &[0, 1]);
        let call = with(0xb6, &[0, 3]);
        let insns = [
            op(ALOAD_0),
            op(ASTORE_1),
            field.clone(),
            op(ALOAD_1),
            call.clone(),
            op(0xb1),
        ];
        assert_eq!(
            rewrite(&insns, &[], &[]),
            Some(vec![op(ALOAD_0), field, op(SWAP), call, op(0xb1)])
        );
    }

    #[test]
    fn a_line_mark_inside_the_swap_pattern_defeats_it() {
        let field = with(GETSTATIC, &[0, 1]);
        let call = with(0xb6, &[0, 3]);
        let insns = [
            op(ALOAD_0),
            op(ASTORE_1),
            field,
            op(ALOAD_1),
            call,
            op(0xb1),
        ];
        assert_eq!(rewrite(&insns, &[], &[2]), None);
    }

    #[test]
    fn an_expression_null_check_keeps_its_value_on_the_stack() {
        let ldc = with(LDC, &[7]);
        let check = with(INVOKESTATIC, &[0, 9]);
        let insns = [
            op(ALOAD_0),
            op(ASTORE_1),
            op(ALOAD_1),
            ldc.clone(),
            check.clone(),
            op(ALOAD_1),
            op(ARETURN),
        ];
        assert_eq!(
            rewrite(&insns, &[], &[]),
            Some(vec![op(ALOAD_0), op(DUP), ldc, check, op(ARETURN)])
        );
    }

    #[test]
    fn an_unread_temporary_is_popped() {
        let insns = [op(ALOAD_0), op(ASTORE_1), op(0xb1)];
        assert_eq!(
            rewrite(&insns, &[], &[]),
            Some(vec![op(ALOAD_0), op(POP), op(0xb1)])
        );
    }

    #[test]
    fn a_load_discarded_at_once_is_removed() {
        let insns = [op(ALOAD_0), op(POP), op(0xb1)];
        assert_eq!(rewrite(&insns, &[], &[]), Some(vec![op(0xb1)]));
    }

    #[test]
    fn a_load_discarded_at_a_protected_start_leaves_a_nop() {
        let insns = [op(ALOAD_0), op(POP), op(0xb1), op(ASTORE_1), op(0xb1)];
        let handlers = [Handler {
            start: 0,
            end: 2,
            handler: 3,
        }];
        assert_eq!(
            rewrite_with(&insns, &[], &[], &[], &handlers),
            Some(vec![op(NOP), op(0xb1), op(ASTORE_1), op(0xb1)])
        );
    }

    #[test]
    fn a_load_discarded_at_a_debug_mark_moves_the_mark_to_what_follows() {
        let insns = [op(ALOAD_0), op(POP), op(0xb1)];
        assert_eq!(
            rewrite_with(&insns, &[], &[0], &[], &[]),
            Some(vec![op(0xb1)])
        );
    }

    #[test]
    fn an_unlabelled_nop_next_to_an_instruction_is_removed() {
        let insns = [op(ALOAD_0), op(NOP), op(ARETURN)];
        assert_eq!(
            rewrite(&insns, &[], &[]),
            Some(vec![op(ALOAD_0), op(ARETURN)])
        );
    }

    #[test]
    fn a_nop_separated_from_both_neighbors_by_labels_is_retained() {
        let insns = [op(ALOAD_0), op(NOP), op(ARETURN)];
        assert_eq!(rewrite(&insns, &[], &[1, 2]), None);
    }

    #[test]
    fn a_safe_call_shape_is_left_for_the_complete_safe_call_transform() {
        let insns = [
            op(ALOAD_0),
            Insn::Branch {
                op: IFNONNULL,
                target: BranchTarget::Internal(3),
            },
            op(0x01), // aconst_null
            op(ARETURN),
        ];
        assert_eq!(rewrite(&insns, &[3], &[]), None);
    }

    #[test]
    fn a_safe_call_exposed_by_nop_cleanup_is_still_left_unchanged() {
        let insns = [
            op(ALOAD_0),
            op(NOP),
            Insn::Branch {
                op: IFNONNULL,
                target: BranchTarget::Internal(4),
            },
            op(0x01), // aconst_null
            op(ARETURN),
        ];
        assert_eq!(rewrite(&insns, &[4], &[]), None);
    }

    #[test]
    fn a_wide_temporary_keeps_its_two_word_value_on_the_stack() {
        let insns = [op(0x09), op(0x40), op(0x1f), op(0xad)]; // lconst_0; lstore_1; lload_1; lreturn
        assert_eq!(rewrite(&insns, &[], &[]), Some(vec![op(0x09), op(0xad)]));
    }

    #[test]
    fn a_catch_store_is_not_a_temporary() {
        let insns = [op(0x01), op(ASTORE_1), op(ALOAD_1), op(ARETURN)];
        let handlers = [Handler {
            start: 0,
            end: 1,
            handler: 1,
        }];
        assert_eq!(rewrite_with(&insns, &[1], &[], &[], &handlers), None);
    }

    #[test]
    fn an_unrecognized_catch_entry_declines_the_rewrite() {
        let insns = [op(ALOAD_0), op(POP), op(0xb1)];
        let handlers = [Handler {
            start: 0,
            end: 2,
            handler: 2,
        }];
        assert_eq!(rewrite_with(&insns, &[], &[], &[], &handlers), None);
    }

    #[test]
    fn analysis_complexity_overflow_and_oversize_are_refused() {
        assert!(analysis_within_limit(10, 4, 3));
        assert!(!analysis_within_limit(ANALYSIS_COMPLEXITY_LIMIT, 2, 1));
        assert!(!analysis_within_limit(usize::MAX, 2, 2));
    }

    #[test]
    fn a_value_read_on_two_paths_from_two_stores_is_not_a_temporary() {
        // 0 iload_0; 1 ifeq 5; 2 aload_0; 3 astore_1; 4 goto 7; 5 aconst_null; 6 astore_1;
        // 7 aload_1; 8 areturn — slot 1 merges two stores at 7.
        let insns = [
            op(0x1a),
            Insn::Branch {
                op: 0x99,
                target: BranchTarget::Internal(5),
            },
            op(ALOAD_0),
            op(ASTORE_1),
            Insn::Branch {
                op: 0xa7,
                target: BranchTarget::Internal(7),
            },
            op(0x01),
            op(ASTORE_1),
            op(ALOAD_1),
            op(ARETURN),
        ];
        assert_eq!(rewrite(&insns, &[5, 7], &[]), None);
    }
}
