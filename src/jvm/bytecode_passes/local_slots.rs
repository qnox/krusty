//! kotlinc's `removeUnusedLocalVariables`, the last step of its final dead-code transformer.
//!
//! A slot is used when it holds `this` or a parameter, when a load, store or `iinc` names it (a
//! `long`/`double` load or store both of its words), or when a local variable entry does (both
//! words of a `long`/`double` entry). When every slot up to the highest used one is used nothing
//! changes. Otherwise every used slot moves down to its rank among the used slots, which closes the
//! gaps a folded temporary or a removed dead store left: `remapLocalVariables` renumbers each
//! load, store, `iinc` and local variable entry. The assembler then writes each access in its
//! shortest form (`aload_3` for slot 3, `wide` only above 255), as ASM does.

use std::collections::BTreeSet;

use crate::jvm::method_node::{Insn, MethodNode, Node};

const LLOAD: u8 = 0x16;
const DLOAD: u8 = 0x18;
const LSTORE: u8 = 0x37;
const DSTORE: u8 = 0x39;

/// The slots `insn` uses: a `long`/`double` load or store both words, any other access its own.
fn touched(insn: &Insn) -> Vec<u16> {
    match *insn {
        Insn::Var {
            op: LLOAD | DLOAD | LSTORE | DSTORE,
            slot,
        } => vec![slot, slot + 1],
        Insn::Var { slot, .. } | Insn::Iinc { slot, .. } => vec![slot],
        _ => Vec::new(),
    }
}

fn slot_mut(insn: &mut Insn) -> Option<&mut u16> {
    match insn {
        Insn::Var { slot, .. } | Insn::Iinc { slot, .. } => Some(slot),
        _ => None,
    }
}

/// Close the gaps among the slots `method`'s accesses, its local variables and `fixed` (`this` and
/// the parameters) use, renumbering every access and local variable; `false` when there is none.
pub(crate) fn compact(method: &mut MethodNode, fixed: &BTreeSet<u16>) -> bool {
    let mut used = fixed.clone();
    used.extend(method.instructions().flat_map(touched));
    for local in &method.local_variables {
        used.insert(local.slot);
        if matches!(local.desc.as_str(), "J" | "D") {
            used.insert(local.slot + 1);
        }
    }
    let Some(&highest) = used.last() else {
        return false;
    };
    if used.len() == usize::from(highest) + 1 {
        return false;
    }
    let mut moved = vec![0; usize::from(highest) + 1];
    for (rank, &slot) in used.iter().enumerate() {
        // Fewer used slots than the highest one, so every rank fits.
        moved[usize::from(slot)] = rank as u16;
    }
    for node in &mut method.nodes {
        if let Node::Insn(insn) = node {
            if let Some(slot) = slot_mut(insn) {
                *slot = moved[usize::from(*slot)];
            }
        }
    }
    for local in &mut method.local_variables {
        local.slot = moved[usize::from(local.slot)];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm::method_node::LocalVariable;

    const ALOAD: u8 = 0x19;
    const ASTORE: u8 = 0x3a;
    const ILOAD: u8 = 0x15;
    const ISTORE: u8 = 0x36;

    fn var(op: u8, slot: u16) -> Insn {
        Insn::Var { op, slot }
    }

    fn run(insns: &[Insn], fixed: &[u16]) -> Option<Vec<Insn>> {
        let mut method = MethodNode::new(0x0009, "f", "()V");
        method.nodes = insns.iter().cloned().map(Node::Insn).collect();
        compact(&mut method, &fixed.iter().copied().collect())
            .then(|| method.instructions().cloned().collect())
    }

    #[test]
    fn a_folded_temporary_leaves_its_slot_to_the_next_local() {
        // `for (e in xs)` after its iterable's temporary (slot 1) was folded.
        let insns = [
            var(ALOAD, 0),
            Insn::Op(0x57),
            var(ASTORE, 2),
            var(ALOAD, 2),
            Insn::Op(0xb0),
        ];
        assert_eq!(
            run(&insns, &[0]),
            Some(vec![
                var(ALOAD, 0),
                Insn::Op(0x57),
                var(ASTORE, 1),
                var(ALOAD, 1),
                Insn::Op(0xb0),
            ])
        );
    }

    #[test]
    fn a_method_whose_slots_are_all_used_is_left_alone() {
        let insns = [
            var(ILOAD, 0),
            var(ISTORE, 1),
            Insn::Iinc { slot: 1, delta: 1 },
            var(ILOAD, 1),
            Insn::Op(0xac),
        ];
        assert_eq!(run(&insns, &[0]), None);
    }

    #[test]
    fn a_long_keeps_both_words() {
        // A long at slots 5-6 over a gap at 1-4 moves to 1-2.
        let insns = [var(LSTORE, 5), var(LLOAD, 5), Insn::Op(0xad)];
        assert_eq!(
            run(&insns, &[0]),
            Some(vec![var(LSTORE, 1), var(LLOAD, 1), Insn::Op(0xad)])
        );
    }

    #[test]
    fn a_named_local_keeps_its_slot_used_and_moves_with_it() {
        // Slot 1 is named by a local variable though no instruction reads it; slot 3 by a long
        // local whose second word is slot 4.
        let mut method = MethodNode::new(0x0009, "f", "()V");
        let (start, end) = (method.new_label(), method.new_label());
        method.nodes = vec![
            Node::Label(start),
            Node::Insn(var(ALOAD, 6)),
            Node::Insn(Insn::Op(0xb0)),
            Node::Label(end),
        ];
        for (slot, desc) in [(1, "I"), (3, "J")] {
            method.local_variables.push(LocalVariable {
                name: "x".to_string(),
                desc: desc.to_string(),
                start,
                end,
                slot,
            });
        }
        assert!(compact(&mut method, &[0].into_iter().collect()));
        assert_eq!(method.instructions().next(), Some(&var(ALOAD, 4)));
        let slots: Vec<u16> = method
            .local_variables
            .iter()
            .map(|local| local.slot)
            .collect();
        assert_eq!(slots, vec![1, 2]);
    }

    #[test]
    fn an_access_above_255_moves_down() {
        let insns = [
            var(ASTORE, 256),
            Insn::Iinc {
                slot: 256,
                delta: 256,
            },
            var(ALOAD, 256),
            Insn::Op(0xb0),
        ];
        assert_eq!(
            run(&insns, &[0]),
            Some(vec![
                var(ASTORE, 1),
                Insn::Iinc {
                    slot: 1,
                    delta: 256
                },
                var(ALOAD, 1),
                Insn::Op(0xb0),
            ])
        );
    }

    #[test]
    fn nothing_used_is_nothing_to_do() {
        assert_eq!(run(&[Insn::Op(0xb1)], &[]), None);
    }
}
