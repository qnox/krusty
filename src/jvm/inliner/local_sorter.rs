//! ASM's `LocalVariablesSorter`, which kotlinc's inliner runs the body through: every local above
//! the parameters gets a fresh slot, handed out in the order the body first names it.
//!
//! A slot is keyed by its index and its size, so a slot the body uses for both an `int` and a
//! `long` becomes two locals. The sorter sees accesses in the order kotlinc's visitor does: the
//! body's instructions, an inlined lambda's instructions and variable table where its `invoke`
//! was, and the body's own variable table last (see `lambda_expansion`).

use crate::jvm::method_node::{Insn, LocalVariable};

const LLOAD: u8 = 0x16;
const DLOAD: u8 = 0x18;
const LSTORE: u8 = 0x37;
const DSTORE: u8 = 0x39;

/// The sorter's state: the first slot it renumbers and the next slot it hands out.
pub(super) struct Sorter {
    first_local: u16,
    next_local: u16,
    /// `2 * slot + size - 1` → the new slot plus one (zero while unassigned).
    remapped: Vec<u16>,
}

impl Sorter {
    /// A sorter that leaves the slots below `first_local` (the parameters' words) where they are.
    pub(super) fn new(first_local: u16) -> Sorter {
        Sorter {
            first_local,
            next_local: first_local,
            remapped: Vec::new(),
        }
    }

    fn remap(&mut self, slot: u16, size: u16) -> u16 {
        if slot + size <= self.first_local {
            return slot;
        }
        let key = (2 * slot + size - 1) as usize;
        if key >= self.remapped.len() {
            self.remapped.resize(key + 1, 0);
        }
        if self.remapped[key] == 0 {
            let local = self.next_local;
            self.next_local += size;
            self.remapped[key] = local + 1;
        }
        self.remapped[key] - 1
    }

    /// Renumber the local an instruction accesses, if it accesses one.
    pub(super) fn visit(&mut self, insn: &mut Insn) {
        match insn {
            Insn::Var { op, slot } => {
                let size = if matches!(*op, LLOAD | DLOAD | LSTORE | DSTORE) {
                    2
                } else {
                    1
                };
                *slot = self.remap(*slot, size);
            }
            Insn::Iinc { slot, .. } => *slot = self.remap(*slot, 1),
            _ => {}
        }
    }

    /// Renumber a local-variable entry's slot.
    pub(super) fn visit_local(&mut self, local: &mut LocalVariable) {
        let size = if matches!(local.desc.as_str(), "J" | "D") {
            2
        } else {
            1
        };
        local.slot = self.remap(local.slot, size);
    }

    /// The method's `max_locals` once every access was visited.
    pub(super) fn max_locals(&self) -> u16 {
        self.next_local
    }
}
