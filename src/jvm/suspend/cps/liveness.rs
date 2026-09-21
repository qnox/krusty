//! Backward live-variable analysis over a decoded method body.
//!
//! The coroutine transform spills exactly the locals that are live across a suspension, so this is
//! the analysis the spill set is read from. It is deliberately a *slot* analysis: a JVM local is a
//! slot, a `long`/`double` occupies two, and nothing here needs to know which Kotlin variable a
//! slot is currently realizing.

use super::ControlGraph;
use crate::jvm::inline::Insn;

/// A set of local-variable slots.
#[derive(Clone, Default, Eq, PartialEq)]
pub(crate) struct SlotSet {
    words: Vec<u64>,
}

impl SlotSet {
    fn ensure(&mut self, slot: u16) {
        let word = slot as usize / 64;
        if self.words.len() <= word {
            self.words.resize(word + 1, 0);
        }
    }

    pub(crate) fn insert(&mut self, slot: u16) {
        self.ensure(slot);
        self.words[slot as usize / 64] |= 1 << (slot as usize % 64);
    }

    pub(crate) fn remove(&mut self, slot: u16) {
        let word = slot as usize / 64;
        if word < self.words.len() {
            self.words[word] &= !(1 << (slot as usize % 64));
        }
    }

    pub(crate) fn contains(&self, slot: u16) -> bool {
        self.words
            .get(slot as usize / 64)
            .is_some_and(|word| word & (1 << (slot as usize % 64)) != 0)
    }

    /// Keep only what both hold; `true` when that removed something.
    pub(crate) fn intersect_with(&mut self, other: &SlotSet) -> bool {
        let mut changed = false;
        for (index, word) in self.words.iter_mut().enumerate() {
            let kept = *word & other.words.get(index).copied().unwrap_or(0);
            changed |= kept != *word;
            *word = kept;
        }
        changed
    }

    /// `true` when this changed.
    fn union_with(&mut self, other: &SlotSet) -> bool {
        if other.words.len() > self.words.len() {
            self.words.resize(other.words.len(), 0);
        }
        let mut changed = false;
        for (mine, &theirs) in self.words.iter_mut().zip(&other.words) {
            let merged = *mine | theirs;
            changed |= merged != *mine;
            *mine = merged;
        }
        changed
    }

    /// Slots in the set, ascending.
    pub(crate) fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.words.iter().enumerate().flat_map(|(word, &bits)| {
            (0..64)
                .filter(move |bit| bits & (1 << bit) != 0)
                .map(move |bit| (word * 64 + bit) as u16)
        })
    }
}

impl std::fmt::Debug for SlotSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.iter()).finish()
    }
}

/// How many slots the local access encoded by `op` occupies (`long`/`double` take two).
fn access_width(op: u8) -> u16 {
    match op {
        0x16 | 0x18 | 0x37 | 0x39 => 2, // lload/dload/lstore/dstore
        0x1e..=0x21 | 0x26..=0x29 => 2, // lload_<n>/dload_<n>
        0x3f..=0x42 | 0x47..=0x4a => 2, // lstore_<n>/dstore_<n>
        _ => 1,
    }
}

/// The inner opcode of a `wide` instruction, or `op` itself.
fn effective_opcode(insn: &Insn) -> Option<u8> {
    match insn {
        Insn::Plain { op: 0xc4, operands } => operands.first().copied(),
        Insn::Plain { op, .. } => Some(*op),
        _ => None,
    }
}

/// The slot an `iinc` (or `wide iinc`) reads and writes.
fn incremented_local(insn: &Insn) -> Option<u16> {
    match insn {
        Insn::Plain { op: 0x84, operands } => operands.first().copied().map(u16::from),
        Insn::Plain { op: 0xc4, operands } if operands.first() == Some(&0x84) => operands
            .get(1)
            .zip(operands.get(2))
            .map(|(&high, &low)| (u16::from(high) << 8) | u16::from(low)),
        _ => None,
    }
}

/// Slots read, then slots written, by one instruction. An `iinc` appears in both.
fn reads_and_writes(insn: &Insn) -> (Vec<u16>, Vec<u16>) {
    if let Some(slot) = incremented_local(insn) {
        return (vec![slot], vec![slot]);
    }
    let Some(op) = effective_opcode(insn) else {
        return (Vec::new(), Vec::new());
    };
    let width = access_width(op);
    let span = |slot: u16| (0..width).map(move |offset| slot + offset).collect();
    if let Some(slot) = crate::jvm::inline::loaded_local(insn) {
        return (span(slot), Vec::new());
    }
    if let Some(slot) = crate::jvm::inline::stored_local(insn) {
        return (Vec::new(), span(slot));
    }
    (Vec::new(), Vec::new())
}

/// Live local slots at every program point of one method body.
pub(crate) struct LocalLiveness {
    /// Indexed by instruction; entry `exit` is the virtual end of the body.
    before: Vec<SlotSet>,
}

impl LocalLiveness {
    /// `None` only when the graph and the instruction list disagree in length — a caller bug.
    pub(crate) fn analyze(insns: &[Insn], graph: &ControlGraph) -> Option<LocalLiveness> {
        if graph.exit() != insns.len() {
            return None;
        }
        let access: Vec<(Vec<u16>, Vec<u16>)> = insns.iter().map(reads_and_writes).collect();
        let mut before = vec![SlotSet::default(); insns.len() + 1];
        // Backward over reverse post-order: a backward analysis converges fastest visiting the
        // reverse of the forward order, and the graph hands that order over directly.
        let mut order = graph.reverse_post_order();
        order.reverse();
        loop {
            let mut changed = false;
            for &index in &order {
                if index == insns.len() {
                    continue; // the exit demands nothing
                }
                let (reads, writes) = &access[index];
                let mut live = SlotSet::default();
                for &successor in graph.normal_successors(index) {
                    live.union_with(&before[successor]);
                }
                for &slot in writes {
                    live.remove(slot);
                }
                // An exceptional edge bypasses the kill: the throw may precede the store.
                for &handler in graph.exceptional_successors(index) {
                    live.union_with(&before[handler]);
                }
                for &slot in reads {
                    live.insert(slot);
                }
                if live != before[index] {
                    before[index] = live;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        Some(LocalLiveness { before })
    }

    /// Slots DEFINITELY assigned before `index` runs: every path from the method's entry to it
    /// stores them.
    ///
    /// The spill plan needs this where a frame cannot be read: a local the SPLICED body assigns —
    /// an inline-depth marker, a loop's own temporary — is described by frames after the suspension
    /// but by none before it. Spilling one that a path could leave unset would fail verification at
    /// the spill itself, so "some store precedes it in the instruction order" is not enough.
    pub(crate) fn definitely_assigned(
        insns: &[Insn],
        graph: &ControlGraph,
        parameters: &SlotSet,
    ) -> Option<Vec<SlotSet>> {
        if graph.exit() != insns.len() {
            return None;
        }
        let access: Vec<(Vec<u16>, Vec<u16>)> = insns.iter().map(reads_and_writes).collect();
        // Everything is assumed assigned everywhere, then cut back to what every edge agrees on —
        // the usual way to reach the greatest fixed point of an intersection.
        let mut all = SlotSet::default();
        for (_, writes) in &access {
            for &slot in writes {
                all.insert(slot);
            }
        }
        for slot in parameters.iter() {
            all.insert(slot);
        }
        let mut before = vec![all.clone(); insns.len() + 1];
        before[0] = parameters.clone();
        let order = graph.reverse_post_order();
        loop {
            let mut changed = false;
            for &index in &order {
                if index == insns.len() {
                    continue;
                }
                let mut out = before[index].clone();
                for &slot in &access[index].1 {
                    out.insert(slot);
                }
                for &successor in graph.normal_successors(index) {
                    if successor != 0 {
                        changed |= before[successor].intersect_with(&out);
                    }
                }
                // A handler is reached from anywhere inside the protected range, including before
                // the store that follows the throw, so an exceptional edge carries the state BEFORE
                // this instruction's own writes.
                let entry_state = before[index].clone();
                for &handler in graph.exceptional_successors(index) {
                    if handler != 0 {
                        changed |= before[handler].intersect_with(&entry_state);
                    }
                }
            }
            if !changed {
                break;
            }
        }
        Some(before)
    }

    /// Slots live immediately BEFORE `index` executes.
    pub(crate) fn live_before(&self, index: usize) -> &SlotSet {
        static EMPTY: std::sync::OnceLock<SlotSet> = std::sync::OnceLock::new();
        self.before
            .get(index)
            .unwrap_or_else(|| EMPTY.get_or_init(SlotSet::default))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::inline::BranchTarget;
    use crate::jvm::suspend::cps::Handler;

    fn plain(op: u8) -> Insn {
        Insn::Plain {
            op,
            operands: Vec::new(),
        }
    }

    fn wide(inner: u8, slot: u16) -> Insn {
        Insn::Plain {
            op: 0xc4,
            operands: vec![inner, (slot >> 8) as u8, (slot & 0xff) as u8],
        }
    }

    fn live(insns: &[Insn], handlers: &[Handler]) -> LocalLiveness {
        let graph = ControlGraph::build(insns, handlers).expect("graph");
        LocalLiveness::analyze(insns, &graph).expect("liveness")
    }

    fn slots(set: &SlotSet) -> Vec<u16> {
        set.iter().collect()
    }

    #[test]
    fn a_local_is_live_from_its_read_back_to_its_store() {
        // astore_1 ; nop ; aload_1 ; areturn
        let insns = [plain(0x4c), plain(0x00), plain(0x2b), plain(0xb0)];
        let liveness = live(&insns, &[]);
        assert_eq!(slots(liveness.live_before(0)), [] as [u16; 0]);
        assert_eq!(slots(liveness.live_before(1)), [1]);
        assert_eq!(slots(liveness.live_before(2)), [1]);
        assert_eq!(slots(liveness.live_before(3)), [] as [u16; 0]);
    }

    #[test]
    fn a_store_kills_the_slot_for_everything_before_it() {
        // aload_1 ; astore_1 — the load keeps it live, the store does not resurrect it
        let insns = [plain(0x4c), plain(0x2b), plain(0xb0)];
        let liveness = live(&insns, &[]);
        assert_eq!(slots(liveness.live_before(0)), [] as [u16; 0]);
    }

    #[test]
    fn a_long_store_covers_both_of_its_slots() {
        // lstore_1 (slots 1,2) ; lload_1 ; lreturn
        let insns = [plain(0x40), plain(0x1f), plain(0xad)];
        let liveness = live(&insns, &[]);
        assert_eq!(slots(liveness.live_before(1)), [1, 2]);
        assert_eq!(slots(liveness.live_before(0)), [] as [u16; 0]);
    }

    #[test]
    fn a_wide_access_names_the_same_slot_as_its_compact_form() {
        // wide astore 300 ; wide aload 300 ; areturn
        let insns = [wide(0x3a, 300), wide(0x19, 300), plain(0xb0)];
        let liveness = live(&insns, &[]);
        assert_eq!(slots(liveness.live_before(1)), [300]);
        assert_eq!(slots(liveness.live_before(0)), [] as [u16; 0]);
    }

    #[test]
    fn iinc_both_reads_and_writes_its_slot() {
        // iinc 1, 1 ; return — the read keeps slot 1 live even though the same instruction writes it
        let insns = [
            Insn::Plain {
                op: 0x84,
                operands: vec![1, 1],
            },
            plain(0xb1),
        ];
        let liveness = live(&insns, &[]);
        assert_eq!(slots(liveness.live_before(0)), [1]);
    }

    #[test]
    fn a_loop_keeps_its_induction_variable_live_around_the_back_edge() {
        // 0: iload_1        read
        // 1: ifeq 4
        // 2: iinc 1, -1
        // 3: goto 0
        // 4: return
        let insns = [
            plain(0x1b),
            Insn::Branch {
                op: 0x99,
                target: BranchTarget::Internal(4),
            },
            Insn::Plain {
                op: 0x84,
                operands: vec![1, 0xff],
            },
            Insn::Branch {
                op: 0xa7,
                target: BranchTarget::Internal(0),
            },
            plain(0xb1),
        ];
        let liveness = live(&insns, &[]);
        for index in 0..4 {
            assert_eq!(slots(liveness.live_before(index)), [1], "at {index}");
        }
        assert_eq!(slots(liveness.live_before(4)), [] as [u16; 0]);
    }

    /// A throw may precede the store that would have killed the slot, so the handler's demand
    /// reaches past it. Killing along the exceptional edge would under-spill and lose the value.
    #[test]
    fn an_exceptional_edge_is_not_filtered_by_the_store_it_leaves_from() {
        // 0: astore_1   (inside the protected range)
        // 1: return
        // 2: aload_1 ; areturn   (handler)
        let insns = [plain(0x4c), plain(0xb1), plain(0x2b), plain(0xb0)];
        let handlers = [Handler {
            start: 0,
            end: 2,
            handler: 2,
        }];
        assert_eq!(slots(live(&insns, &handlers).live_before(0)), [1]);
        assert_eq!(slots(live(&insns, &[]).live_before(0)), [] as [u16; 0]);
    }
}
