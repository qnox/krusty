//! ASM's `LocalVariablesSorter`, which kotlinc's inliner runs the body through: every local above
//! the parameters gets a fresh slot, handed out in the order the body first names it.
//!
//! A slot is keyed by its index and its size, so a slot the body uses for both an `int` and a
//! `long` becomes two locals. The instructions are visited first, then the variable table.

use crate::jvm::method_node::{Insn, MethodNode, Node};

const LLOAD: u8 = 0x16;
const DLOAD: u8 = 0x18;
const LSTORE: u8 = 0x37;
const DSTORE: u8 = 0x39;

struct Sorter {
    first_local: u16,
    next_local: u16,
    /// `2 * slot + size - 1` → the new slot plus one (zero while unassigned).
    remapped: Vec<u16>,
}

impl Sorter {
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
}

/// Renumber `node`'s locals above `first_local` (the parameters' words) and set its `max_locals`.
pub(super) fn sort(node: &mut MethodNode, first_local: u16) {
    let mut sorter = Sorter {
        first_local,
        next_local: first_local,
        remapped: Vec::new(),
    };
    for entry in &mut node.nodes {
        match entry {
            Node::Insn(Insn::Var { op, slot }) => {
                let size = if matches!(*op, LLOAD | DLOAD | LSTORE | DSTORE) {
                    2
                } else {
                    1
                };
                *slot = sorter.remap(*slot, size);
            }
            Node::Insn(Insn::Iinc { slot, .. }) => *slot = sorter.remap(*slot, 1),
            _ => {}
        }
    }
    for local in &mut node.local_variables {
        let size = if matches!(local.desc.as_str(), "J" | "D") {
            2
        } else {
            1
        };
        local.slot = sorter.remap(local.slot, size);
    }
    node.max_locals = sorter.next_local;
}
