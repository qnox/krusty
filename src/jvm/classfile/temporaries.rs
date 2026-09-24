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
//! Before those, kotlinc's null-check rules (`simplifyKnownSafeCallPatterns`) keep a checked value
//! on the stack for the path that loads it again:
//!
//! - `aload x; ifnonnull L; …; L: aload x` (L reached only by that jump) becomes
//!   `aload x; dup; ifnonnull L; pop; …; L:`;
//! - `aload x; ifnull L; aload x` becomes `aload x; dup; ifnull L`, when EVERY jump to L has that
//!   shape and nothing falls into L, which then begins with a `pop`.
//!
//! Either load of `x` after the jump may instead follow an `aload y` or a one-word `getstatic`,
//! which a `swap` then puts under the kept value. The jump target's frame gains that value on its
//! stack.
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
    /// `true` for each `checkcast` kotlinc's redundant-cast pass removes. That pass runs before this
    /// one, so selected instructions are gone before any rule here looks.
    pub redundant_casts: &'a [bool],
    /// `true` for each instruction removed with a redundant null check and its feed. This pass also
    /// runs before temporary elimination.
    pub redundant_null_checks: &'a [bool],
    /// The label each branch jumps to, by original index, where the builder recorded one. Several
    /// labels can stand at one index; kotlinc's rules tell them apart.
    pub branch_labels: &'a [Option<u32>],
    /// The labels bound at each original index, in the order they stand (length `insns.len() + 1`).
    pub labels_at: &'a [Vec<u32>],
    /// Whether a `getstatic` operand's field is one JVM word.
    pub one_word_static: &'a dyn Fn(u16) -> bool,
    /// Whether an `ldc`/`ldc_w` operand is a `String` constant.
    pub string_constant: &'a dyn Fn(u16) -> bool,
    /// Whether an `invokestatic` operand is `Intrinsics.checkNotNullExpressionValue` or the older
    /// `checkExpressionValueIsNotNull`.
    pub expression_null_check: &'a dyn Fn(u16) -> bool,
}

/// The rewritten body.
pub(crate) struct Rewrite {
    pub nodes: Vec<(Insn, Placement)>,
    /// Branch targets a null-check rule left a value on the stack at: `(original index the
    /// target stood at, original indices of the loads whose value arrives there)`. The value's type
    /// is the loaded local's before each of those loads.
    pub stack_at_target: Vec<(usize, Vec<usize>)>,
    /// Labels that stand AFTER an instruction a rule inserted right behind an earlier label at their
    /// index: kotlinc's `L: pop; E:`, where `L` is a null check's target and `E` a later label bound
    /// at the same offset. Their branches and frames land after the inserted instruction.
    pub late_labels: BTreeSet<u32>,
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
const GOTO: u8 = 0xa7;
const ATHROW: u8 = 0xbf;

/// Same 50 MiB-by-method complexity ceiling as kotlinc's optimizer. The product is deliberately
/// conservative for this representation: a slot/store analysis cell is larger than one byte.
const ANALYSIS_COMPLEXITY_LIMIT: usize = 50 * 1024 * 1024;

fn analysis_within_limit(instructions: usize, slots: usize, candidates: usize) -> bool {
    instructions
        .checked_mul(slots)
        .and_then(|cells| cells.checked_mul(candidates.max(1)))
        .is_some_and(|cells| cells <= ANALYSIS_COMPLEXITY_LIMIT)
}

/// The opcode and original target index of a two-byte branch.
fn jump(insn: &Insn) -> Option<(u8, usize)> {
    match insn {
        Insn::Branch {
            op,
            target: BranchTarget::Internal(to),
        } => Some((*op, *to)),
        _ => None,
    }
}

/// Where an instruction inserted right after the node at `placement` goes.
fn after(placement: Placement) -> Placement {
    match placement {
        Placement::Before(k) => Placement::Before(k),
        Placement::Original(k) | Placement::After(k) => Placement::After(k),
    }
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

    /// Original instruction groups whose labels now stand in front of node `at`. Earlier passes can
    /// remove a whole group, so this includes every skipped group since the preceding live node.
    fn boundary_groups(&self, at: usize) -> std::ops::RangeInclusive<usize> {
        let group = self.nodes[at].1.group();
        let first = at.checked_sub(1).map_or(0, |previous| {
            self.nodes[previous].1.group().saturating_add(1)
        });
        first..=group
    }

    /// Whether a label kotlinc keeps stands in front of node `at`.
    fn labelled(&self, at: usize) -> bool {
        self.boundary_groups(at)
            .any(|group| self.body.arrivals[group] || self.body.marks[group])
    }

    /// Whether a label with non-trivial predecessors stands in front of node `at`.
    fn arrived(&self, at: usize) -> bool {
        self.boundary_groups(at)
            .any(|group| self.body.arrivals[group])
    }

    /// `nodes[at..at + ops.len()]` match `ops` with nothing kotlinc keeps between them.
    fn raw_sequence(&self, at: usize, len: usize) -> bool {
        at + len <= self.nodes.len() && (at + 1..at + len).all(|next| !self.labelled(next))
    }

    /// The first node a label at original index `group` lands on.
    fn group_start(&self, group: usize) -> usize {
        self.nodes
            .iter()
            .position(|(_, placement)| placement.group() >= group)
            .unwrap_or(self.nodes.len())
    }

    /// Whether nothing kotlinc keeps in front of the node after `at` falls into it — its test for
    /// a label's fall-through predecessor (`goto`, a return, `athrow`) as it applies to kotlinc's
    /// own instruction list. kotlinc writes a `return` expression as `xreturn; nop` and removes the
    /// dead `nop` only in a later pass, so a label right after a return has that `nop` as its
    /// predecessor unless a `goto` (a `when` branch that is not the last) follows it; krusty does
    /// not write that dead tail, so a return is taken to fall through. `athrow` has no tail.
    fn ends_flow(&self, at: usize) -> bool {
        match &self.nodes[at].0 {
            Insn::Branch { op, .. } => *op == GOTO,
            Insn::Plain { op, .. } => *op == ATHROW,
            _ => false,
        }
    }

    /// Every node that jumps to original index `target`, or `None` when something besides
    /// two-byte branches reaches it: a switch, a handler, a protected range's bound, or the
    /// fall-through of the node in front of it.
    fn only_jumps_to(&self, target: usize) -> Option<Vec<usize>> {
        let start = self.group_start(target);
        if start == 0 || !self.ends_flow(start - 1) {
            return None;
        }
        if self.body.handlers.iter().any(|handler| {
            handler.start == target || handler.end == target || handler.handler == target
        }) {
            return None;
        }
        let mut jumps = Vec::new();
        for (at, (insn, _)) in self.nodes.iter().enumerate() {
            match insn {
                Insn::Branch {
                    target: BranchTarget::Internal(to),
                    ..
                } if *to == target => jumps.push(at),
                Insn::BranchW {
                    target: BranchTarget::Internal(to),
                    ..
                } if *to == target => return None,
                Insn::TableSwitch {
                    default, targets, ..
                } if *default == target || targets.contains(&target) => return None,
                Insn::LookupSwitch { default, pairs }
                    if *default == target || pairs.iter().any(|&(_, to)| to == target) =>
                {
                    return None
                }
                _ => {}
            }
        }
        Some(jumps)
    }

    /// Insert each `(position, insn, placement)` in front of the node now at `position`, in the
    /// order given, and drop the nodes at `removed`.
    fn apply(&mut self, mut inserts: Vec<(usize, Insn, Placement)>, removed: &[usize]) {
        let mut nodes = Vec::with_capacity(self.nodes.len() + inserts.len());
        // A stable sort: inserts at one position keep their order.
        inserts.sort_by_key(|(position, _, _)| *position);
        let mut pending = inserts.into_iter().peekable();
        for (at, node) in std::mem::take(&mut self.nodes).into_iter().enumerate() {
            while let Some((_, insn, placement)) =
                pending.next_if(|(position, _, _)| *position == at)
            {
                nodes.push((insn, placement));
            }
            if !removed.contains(&at) {
                nodes.push(node);
            }
        }
        nodes.extend(pending.map(|(_, insn, placement)| (insn, placement)));
        self.nodes = nodes;
    }

    /// The fall-through after the null check at `jump` reloads the checked slot: directly, or
    /// after an `aload` or a one-word `getstatic` it can be swapped under. The reload's position,
    /// and the node a `swap` goes after.
    fn reload(&self, from: usize, slot: u16) -> Option<(usize, Option<usize>)> {
        let reads = |at: usize| {
            matches!(
                self.nodes.get(at).and_then(|(insn, _)| var_op(insn)),
                Some(VarOp::Load(Kind::Reference, s)) if s == slot
            )
        };
        if from >= self.nodes.len() {
            return None;
        }
        if reads(from) {
            return Some((from, None));
        }
        let first = &self.nodes[from].0;
        let swappable = matches!(var_op(first), Some(VarOp::Load(Kind::Reference, _)))
            || (opcode(first) == Some(GETSTATIC)
                && u2_operand(first).is_some_and(|field| (self.body.one_word_static)(field)));
        (swappable && self.raw_sequence(from, 2) && reads(from + 1))
            .then_some((from + 1, Some(from)))
    }

    fn remove(&mut self, positions: &mut [usize]) {
        positions.sort_unstable();
        for &at in positions.iter().rev() {
            self.nodes.remove(at);
        }
    }
}

/// kotlinc's `simplifyKnownSafeCallPatterns`: a null check whose value the path after it loads
/// again keeps that value on the stack. Returns the original indices of the loads it removed; each
/// jump target that now receives a value is added to `stack_at_target`.
fn fold_null_checks(
    working: &mut Working,
    stack_at_target: &mut Vec<(usize, Vec<usize>)>,
    late_labels: &mut BTreeSet<u32>,
) -> Option<BTreeSet<usize>> {
    let insns = working.body.insns;
    let is_candidate = |pair: &[(Insn, Placement)]| {
        matches!(var_op(&pair[0].0), Some(VarOp::Load(Kind::Reference, _)))
            && matches!(jump(&pair[1].0), Some((IFNULL | IFNONNULL, _)))
    };
    let candidates = working
        .nodes
        .windows(2)
        .filter(|pair| is_candidate(pair))
        .count();
    if candidates == 0 {
        return Some(BTreeSet::new());
    }
    // Each candidate may inspect every branch into its target and rebuild the working list. Keep
    // the same per-method ceiling as the temporary-value data-flow analysis rather than allowing
    // a large generated method to turn this pre-pass quadratic without bound.
    if !analysis_within_limit(insns.len(), candidates, 1) {
        return None;
    }
    let mut removed = BTreeSet::new();
    for (load, insn) in insns.iter().enumerate() {
        if !matches!(var_op(insn), Some(VarOp::Load(Kind::Reference, _))) {
            continue;
        }
        let Some(at) = working.position(load) else {
            continue;
        };
        if !working.nodes.get(at..at + 2).is_some_and(is_candidate) {
            continue;
        }
        let Some(VarOp::Load(_, slot)) = var_op(&working.nodes[at].0) else {
            continue;
        };
        if !working.raw_sequence(at, 2) {
            continue;
        }
        let Some((op, target)) = jump(&working.nodes[at + 1].0) else {
            continue;
        };
        let Some(jumps) = working.only_jumps_to(target) else {
            continue;
        };
        if op == IFNONNULL {
            // The jump is the target's only predecessor, and the target reloads the value first
            // thing: no line number or other label stands there.
            let start = working.group_start(target);
            if jumps != [at + 1]
                || working.body.marks[target]
                || working
                    .nodes
                    .get(start)
                    .map(|(_, placement)| placement.group())
                    != Some(target)
            {
                continue;
            }
            let Some((reload, swap_after)) = working.reload(start, slot) else {
                continue;
            };
            let Placement::Original(reloaded) = working.nodes[reload].1 else {
                continue;
            };
            let jump_placement = working.nodes[at + 1].1;
            let mut inserts = vec![
                (
                    at + 1,
                    plain(DUP),
                    Placement::Before(jump_placement.group()),
                ),
                (at + 2, plain(POP), after(jump_placement)),
            ];
            if let Some(under) = swap_after {
                inserts.push((under + 1, plain(SWAP), after(working.nodes[under].1)));
            }
            working.apply(inserts, &[reload]);
            removed.insert(reloaded);
            stack_at_target.push((target, vec![load]));
            continue;
        }
        // `ifnull`: every jump to the target LABEL checks a local and reloads it on the
        // fall-through. Of the labels at the target's offset, one bound before it is a predecessor
        // that is not such a jump; one bound after it is not a predecessor at all, and lands after
        // the `pop` inserted behind the target.
        let label_of = |working: &Working, at: usize| match working.nodes[at].1 {
            Placement::Original(index) => working.body.branch_labels.get(index).copied().flatten(),
            _ => None,
        };
        let (jumps, later) = match label_of(working, at + 1) {
            None => (jumps, Vec::new()),
            Some(own) => {
                let order = &working.body.labels_at[target];
                let Some(rank) = order.iter().position(|&label| label == own) else {
                    continue;
                };
                let mut to_own = Vec::new();
                let mut foreign = false;
                for &jump_at in &jumps {
                    match label_of(working, jump_at) {
                        Some(label) if label == own => to_own.push(jump_at),
                        Some(label)
                            if order.iter().position(|&other| other == label) > Some(rank) => {}
                        _ => foreign = true,
                    }
                }
                if foreign {
                    continue;
                }
                (to_own, order[rank + 1..].to_vec())
            }
        };
        let mut parts = Vec::with_capacity(jumps.len());
        for &jump_at in &jumps {
            let checked = jump_at.checked_sub(1).filter(|&checked| {
                matches!(jump(&working.nodes[jump_at].0), Some((IFNULL, _)))
                    && working.raw_sequence(checked, 2)
            });
            let Some(checked) = checked else {
                break;
            };
            let (
                Some(VarOp::Load(Kind::Reference, checked_slot)),
                Placement::Original(checked_index),
            ) = (var_op(&working.nodes[checked].0), working.nodes[checked].1)
            else {
                break;
            };
            if jump_at + 1 >= working.nodes.len() || working.labelled(jump_at + 1) {
                break;
            }
            let Some((reload, swap_after)) = working.reload(jump_at + 1, checked_slot) else {
                break;
            };
            let Placement::Original(reloaded) = working.nodes[reload].1 else {
                break;
            };
            parts.push((jump_at, checked_index, reload, reloaded, swap_after));
        }
        if parts.len() != jumps.len() {
            continue;
        }
        let mut inserts = Vec::new();
        let mut reloads = Vec::new();
        for &(jump_at, _, reload, reloaded, swap_after) in &parts {
            let jump_placement = working.nodes[jump_at].1;
            inserts.push((
                jump_at,
                plain(DUP),
                Placement::Before(jump_placement.group()),
            ));
            if let Some(under) = swap_after {
                inserts.push((under + 1, plain(SWAP), after(working.nodes[under].1)));
            }
            reloads.push(reload);
            removed.insert(reloaded);
        }
        inserts.push((
            working.group_start(target),
            plain(POP),
            Placement::Before(target),
        ));
        working.apply(inserts, &reloads);
        late_labels.extend(later);
        stack_at_target.push((
            target,
            parts.iter().map(|&(_, checked, ..)| checked).collect(),
        ));
    }
    // The existing temporary/NOP cleanup must not apply only half of kotlinc's coupled safe-call
    // transformation. If a candidate could not be proven foldable, preserve the original method.
    if working.nodes.windows(2).any(is_candidate) {
        return None;
    }
    Some(removed)
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
    let earlier_removed =
        body.redundant_casts.contains(&true) || body.redundant_null_checks.contains(&true);
    working.nodes.retain(|(_, placement)| {
        !matches!(placement, Placement::Original(index)
            if body.redundant_casts.get(*index).copied().unwrap_or(false)
                || body.redundant_null_checks.get(*index).copied().unwrap_or(false))
    });
    let earlier_passes_only = {
        let nodes = working.nodes.clone();
        move || {
            earlier_removed.then(|| Rewrite {
                nodes,
                stack_at_target: Vec::new(),
                late_labels: BTreeSet::new(),
            })
        }
    };
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
        let protected_start = working
            .boundary_groups(at)
            .any(|group| body.handlers.iter().any(|handler| handler.start == group));
        let meaningful_before = at > 0 && !working.labelled(at);
        let meaningful_after = at + 1 < working.nodes.len() && !working.labelled(at + 1);
        if !protected_start && (meaningful_before || meaningful_after) {
            working.remove(&mut [at]);
            removed_nop = true;
        } else {
            at += 1;
        }
    }
    let mut stack_at_target = Vec::new();
    let mut late_labels = BTreeSet::new();
    // The temporary analysis runs over the original instruction indices. Feed it every load that
    // an earlier pass has already selected for removal; otherwise a deleted `aload` that supplied a
    // redundant null check remains a phantom use and keeps its compiler temporary alive.
    let mut removed = trivially_removed;
    removed.extend(
        body.redundant_null_checks
            .iter()
            .enumerate()
            .filter_map(|(index, &is_removed)| is_removed.then_some(index)),
    );
    let Some(folded) = fold_null_checks(&mut working, &mut stack_at_target, &mut late_labels)
    else {
        return earlier_passes_only();
    };
    removed.extend(folded);
    let Some(temporaries) = temporaries(body, &removed) else {
        return earlier_passes_only();
    };
    let mut changed = earlier_removed || removed_nop || !removed.is_empty();
    for (store, loads) in temporaries {
        let Some(store_at) = working.position(store) else {
            continue;
        };
        let Some(VarOp::Store(kind, _)) = var_op(&working.nodes[store_at].0) else {
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
                changed = true;
            }
            _ => {}
        }
    }
    changed.then_some(Rewrite {
        nodes: working.nodes,
        stack_at_target,
        late_labels,
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

    type Stacks = Vec<(usize, Vec<usize>)>;

    fn rewrite_with_stacks(
        insns: &[Insn],
        arrivals: &[usize],
        marks: &[usize],
        named: &[(usize, usize, u16)],
        handlers: &[Handler],
    ) -> Option<(Vec<Insn>, Stacks)> {
        let mut arrival = vec![false; insns.len() + 1];
        for &index in arrivals {
            arrival[index] = true;
        }
        let mut mark = vec![false; insns.len() + 1];
        for &index in marks {
            mark[index] = true;
        }
        let branch_labels = vec![None; insns.len()];
        let labels_at = vec![Vec::new(); insns.len() + 1];
        let redundant_casts = vec![false; insns.len()];
        let body = Body {
            insns,
            handlers,
            arrivals: &arrival,
            marks: &mark,
            named,
            redundant_casts: &redundant_casts,
            redundant_null_checks: &redundant_casts,
            branch_labels: &branch_labels,
            labels_at: &labels_at,
            one_word_static: &|field| field == 1,
            string_constant: &|index| index == 7,
            expression_null_check: &|method| method == 9,
        };
        eliminate(&body).map(|rewrite| {
            (
                rewrite.nodes.into_iter().map(|(insn, _)| insn).collect(),
                rewrite.stack_at_target,
            )
        })
    }

    fn rewrite_with(
        insns: &[Insn],
        arrivals: &[usize],
        marks: &[usize],
        named: &[(usize, usize, u16)],
        handlers: &[Handler],
    ) -> Option<Vec<Insn>> {
        rewrite_with_stacks(insns, arrivals, marks, named, handlers).map(|(insns, _)| insns)
    }

    fn rewrite_stacks(
        insns: &[Insn],
        arrivals: &[usize],
        marks: &[usize],
    ) -> Option<(Vec<Insn>, Stacks)> {
        rewrite_with_stacks(insns, arrivals, marks, &[], &[])
    }

    fn branch(op: u8, to: usize) -> Insn {
        Insn::Branch {
            op,
            target: BranchTarget::Internal(to),
        }
    }

    fn rewrite(insns: &[Insn], arrivals: &[usize], marks: &[usize]) -> Option<Vec<Insn>> {
        rewrite_with(insns, arrivals, marks, &[], &[])
    }

    fn rewrite_after_earlier_passes(
        insns: &[Insn],
        casts: &[usize],
        null_checks: &[usize],
        arrivals: &[usize],
        marks: &[usize],
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
        let mut redundant_casts = vec![false; insns.len()];
        for &index in casts {
            redundant_casts[index] = true;
        }
        let mut redundant_null_checks = vec![false; insns.len()];
        for &index in null_checks {
            redundant_null_checks[index] = true;
        }
        let branch_labels = vec![None; insns.len()];
        let labels_at = vec![Vec::new(); insns.len() + 1];
        let body = Body {
            insns,
            handlers,
            arrivals: &arrival,
            marks: &mark,
            named: &[],
            redundant_casts: &redundant_casts,
            redundant_null_checks: &redundant_null_checks,
            branch_labels: &branch_labels,
            labels_at: &labels_at,
            one_word_static: &|_| false,
            string_constant: &|_| false,
            expression_null_check: &|_| false,
        };
        eliminate(&body).map(|rewrite| rewrite.nodes.into_iter().map(|(insn, _)| insn).collect())
    }

    const ALOAD_0: u8 = 0x2a;
    const ALOAD_1: u8 = 0x2b;
    const ASTORE_1: u8 = 0x4c;
    const ARETURN: u8 = 0xb0;

    #[test]
    fn a_cast_removal_survives_when_the_temporary_pass_declines() {
        let cast = with(0xc0, &[0, 1]);
        let insns = [
            op(0x01), // aconst_null
            cast,
            op(POP),
            op(ALOAD_0),
            branch(IFNULL, 6),
            op(0xb1),
            op(0xb1),
        ];
        assert_eq!(
            rewrite_after_earlier_passes(&insns, &[1], &[], &[6], &[], &[]),
            Some(vec![
                op(0x01),
                op(POP),
                op(ALOAD_0),
                branch(IFNULL, 6),
                op(0xb1),
                op(0xb1),
            ])
        );
    }

    fn safe_call_with_cast_boundary(arrivals: &[usize], marks: &[usize]) -> Vec<Insn> {
        let call = with(0xb6, &[0, 3]);
        let insns = [
            op(ALOAD_0),
            op(ASTORE_1),
            op(ALOAD_1),
            with(0xc0, &[0, 4]),
            branch(IFNULL, 8),
            op(ALOAD_1),
            call.clone(),
            branch(GOTO, 8),
            op(0xb1),
        ];
        let rewritten =
            rewrite_after_earlier_passes(&insns, &[3], &[], arrivals, marks, &[]).expect("cast");
        assert_eq!(
            rewritten,
            vec![
                op(ALOAD_0),
                op(ASTORE_1),
                op(ALOAD_1),
                branch(IFNULL, 8),
                op(ALOAD_1),
                call,
                branch(GOTO, 8),
                op(0xb1),
            ]
        );
        rewritten
    }

    #[test]
    fn a_removed_cast_keeps_a_debug_boundary_for_the_next_pass() {
        safe_call_with_cast_boundary(&[8], &[3]);
    }

    #[test]
    fn a_removed_cast_keeps_a_branch_boundary_for_the_next_pass() {
        safe_call_with_cast_boundary(&[3, 8], &[]);
    }

    #[test]
    fn a_removed_cast_keeps_a_handler_boundary_for_the_next_pass() {
        let cast = with(0xc0, &[0, 1]);
        let insns = [op(0x01), cast, op(NOP), op(POP), op(0xb1)];
        let handler = Handler {
            start: 1,
            end: 4,
            handler: 4,
        };
        assert_eq!(
            rewrite_after_earlier_passes(&insns, &[1], &[], &[1, 4], &[], &[handler]),
            Some(vec![op(0x01), op(NOP), op(POP), op(0xb1)])
        );
    }

    #[test]
    fn a_removed_null_check_keeps_its_debug_boundary_for_the_next_pass() {
        let call = with(0xb6, &[0, 3]);
        let check = with(INVOKESTATIC, &[0, 9]);
        let insns = [
            op(ALOAD_0),
            op(ASTORE_1),
            op(ALOAD_1),
            op(DUP),
            check,
            branch(IFNULL, 9),
            op(ALOAD_1),
            call.clone(),
            branch(GOTO, 9),
            op(0xb1),
        ];
        assert_eq!(
            rewrite_after_earlier_passes(&insns, &[], &[3, 4], &[9], &[3], &[]),
            Some(vec![
                op(ALOAD_0),
                op(ASTORE_1),
                op(ALOAD_1),
                branch(IFNULL, 9),
                op(ALOAD_1),
                call,
                branch(GOTO, 9),
                op(0xb1),
            ])
        );
    }

    #[test]
    fn a_load_removed_with_a_null_check_does_not_keep_its_temporary_alive() {
        let check = with(INVOKESTATIC, &[0, 9]);
        let insns = [op(ALOAD_0), op(ASTORE_1), op(ALOAD_1), check, op(0xb1)];
        assert_eq!(
            rewrite_after_earlier_passes(&insns, &[], &[2, 3], &[], &[], &[]),
            Some(vec![op(ALOAD_0), op(POP), op(0xb1)])
        );
    }

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

    #[test]
    fn a_value_checked_non_null_stays_on_the_stack_for_its_target() {
        // `s?.length`: 0 aload_0; 1 astore_1; 2 aload_1; 3 ifnonnull 6; 4 aconst_null; 5 goto 8;
        // 6 aload_1; 7 invokevirtual; 8 areturn.
        let call = with(0xb6, &[0, 3]);
        let insns = [
            op(ALOAD_0),
            op(ASTORE_1),
            op(ALOAD_1),
            branch(IFNONNULL, 6),
            op(0x01),
            branch(GOTO, 8),
            op(ALOAD_1),
            call.clone(),
            op(ARETURN),
        ];
        assert_eq!(
            rewrite_stacks(&insns, &[6, 8], &[]),
            Some((
                vec![
                    op(ALOAD_0),
                    op(DUP),
                    branch(IFNONNULL, 6),
                    op(POP),
                    op(0x01),
                    branch(GOTO, 8),
                    call,
                    op(ARETURN),
                ],
                vec![(6, vec![2])],
            ))
        );
    }

    #[test]
    fn a_line_number_at_the_non_null_target_keeps_the_reload() {
        let insns = [
            op(ALOAD_0),
            branch(IFNONNULL, 4),
            op(0x01),
            op(ATHROW),
            op(ALOAD_0),
            op(ARETURN),
        ];
        assert_eq!(rewrite(&insns, &[4], &[4]), None);
        assert!(rewrite(&insns, &[4], &[]).is_some());
    }

    #[test]
    fn a_return_in_front_of_the_non_null_target_keeps_the_reload() {
        // `if (s == null) return 0; return s.length`: kotlinc's `ireturn` is followed by a dead
        // `nop` that falls into the target, so the jump is not its only predecessor.
        let call = with(0xb6, &[0, 3]);
        let insns = [
            op(ALOAD_0),
            branch(IFNONNULL, 4),
            op(0x03),
            op(0xac),
            op(ALOAD_0),
            call,
            op(0xac),
        ];
        assert_eq!(rewrite(&insns, &[4], &[]), None);
    }

    #[test]
    fn a_non_null_target_reloading_under_a_static_swaps_it_into_place() {
        let field = with(GETSTATIC, &[0, 1]);
        let call = with(0xb6, &[0, 3]);
        let insns = [
            op(ALOAD_0),
            branch(IFNONNULL, 4),
            op(0x01),
            op(ATHROW),
            field.clone(),
            op(ALOAD_0),
            call.clone(),
            op(0xb1),
        ];
        assert_eq!(
            rewrite(&insns, &[4], &[]),
            Some(vec![
                op(ALOAD_0),
                op(DUP),
                branch(IFNONNULL, 4),
                op(POP),
                op(0x01),
                op(ATHROW),
                field,
                op(SWAP),
                call,
                op(0xb1),
            ])
        );
    }

    #[test]
    fn null_checks_sharing_a_target_all_keep_their_values() {
        // `b?.next?.name` folded onto one null label: 0 aload_0; 1 ifnull 10; 2 aload_0;
        // 3 invokevirtual next; 4 astore_1; 5 aload_1; 6 ifnull 10; 7 aload_1; 8 invokevirtual name;
        // 9 goto 11; 10 aconst_null; 11 areturn.
        let next = with(0xb6, &[0, 3]);
        let name = with(0xb6, &[0, 4]);
        let insns = [
            op(ALOAD_0),
            branch(IFNULL, 10),
            op(ALOAD_0),
            next.clone(),
            op(ASTORE_1),
            op(ALOAD_1),
            branch(IFNULL, 10),
            op(ALOAD_1),
            name.clone(),
            branch(GOTO, 11),
            op(0x01),
            op(ARETURN),
        ];
        assert_eq!(
            rewrite_stacks(&insns, &[10, 11], &[]),
            Some((
                vec![
                    op(ALOAD_0),
                    op(DUP),
                    branch(IFNULL, 10),
                    next,
                    op(DUP),
                    branch(IFNULL, 10),
                    name,
                    branch(GOTO, 11),
                    op(POP),
                    op(0x01),
                    op(ARETURN),
                ],
                vec![(10, vec![0, 5])],
            ))
        );
    }

    #[test]
    fn a_null_target_also_reached_by_falling_through_is_left_alone() {
        // `if (x != null) x.run()`: the call falls into the target the check jumps to.
        let call = with(0xb6, &[0, 3]);
        let insns = [op(ALOAD_0), branch(IFNULL, 4), op(ALOAD_0), call, op(0xb1)];
        assert_eq!(rewrite(&insns, &[4], &[]), None);
    }

    #[test]
    fn a_large_method_without_null_check_candidates_keeps_existing_cleanup() {
        let mut insns = vec![op(ALOAD_0); 8_000];
        insns.extend([op(ALOAD_0), op(POP), op(ARETURN)]);
        let rewritten = rewrite(&insns, &[], &[]).expect("the load/pop cleanup still applies");
        assert_eq!(rewritten.len(), 8_001);
        assert_eq!(rewritten.last(), Some(&op(ARETURN)));
    }

    #[test]
    fn a_label_bound_after_the_null_target_lands_after_its_pop() {
        // `n?.touch()` as a statement: 0 aload_0; 1 astore_1; 2 aload_1; 3 ifnull L; 4 aload_1;
        // 5 invokevirtual; 6 goto E; 7 return, with `L` (label 1) then `E` (label 0) bound at 7.
        // The `goto` jumps to `E`, not to `L`, so `L`'s only predecessor is the null check.
        let call = with(0xb6, &[0, 3]);
        let insns = [
            op(ALOAD_0),
            op(ASTORE_1),
            op(ALOAD_1),
            branch(IFNULL, 7),
            op(ALOAD_1),
            call.clone(),
            branch(GOTO, 7),
            op(0xb1),
        ];
        let arrivals = {
            let mut arrivals = vec![false; insns.len() + 1];
            arrivals[7] = true;
            arrivals
        };
        let marks = vec![false; insns.len() + 1];
        let mut branch_labels = vec![None; insns.len()];
        branch_labels[3] = Some(1);
        branch_labels[6] = Some(0);
        let mut labels_at = vec![Vec::new(); insns.len() + 1];
        labels_at[7] = vec![1, 0];
        let redundant_casts = vec![false; insns.len()];
        let body = Body {
            insns: &insns,
            handlers: &[],
            arrivals: &arrivals,
            marks: &marks,
            named: &[],
            redundant_casts: &redundant_casts,
            redundant_null_checks: &redundant_casts,
            branch_labels: &branch_labels,
            labels_at: &labels_at,
            one_word_static: &|_| false,
            string_constant: &|_| false,
            expression_null_check: &|_| false,
        };
        let rewrite = eliminate(&body).expect("folds");
        assert_eq!(
            rewrite
                .nodes
                .into_iter()
                .map(|(insn, _)| insn)
                .collect::<Vec<_>>(),
            vec![
                op(ALOAD_0),
                op(DUP),
                branch(IFNULL, 7),
                call,
                branch(GOTO, 7),
                op(POP),
                op(0xb1),
            ]
        );
        assert_eq!(rewrite.late_labels, BTreeSet::from([0]));
    }
}
