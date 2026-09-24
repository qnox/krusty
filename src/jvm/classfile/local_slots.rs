//! kotlinc's `removeUnusedLocalVariables`, the last step of its final dead-code transformer.
//!
//! A slot is used when it holds `this` or a parameter, when a load, store or `iinc` names it (a
//! `long`/`double` load or store both of its words), or when a `LocalVariableTable` entry does (both
//! words of a `long`/`double` entry). When every slot up to the highest used one is used nothing
//! changes. Otherwise every used slot moves down to its rank among the used slots, which closes the
//! gaps a folded temporary or a removed dead store left: `remapLocalVariables` renumbers each
//! load, store, `iinc` and local variable entry, and ASM then writes each access in its shortest
//! form (`aload_3` for slot 3, `wide` only above 255).

use std::collections::BTreeSet;

use super::temporaries::Placement;
use crate::jvm::inline::Insn;

const IINC: u8 = 0x84;
const WIDE: u8 = 0xc4;

/// A load or store of type `kind` (`0` int, `1` long, `2` float, `3` double, `4` reference), or an
/// `iinc`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Load { kind: u8, slot: u16 },
    Store { kind: u8, slot: u16 },
    Iinc { slot: u16, by: i16 },
}

impl Access {
    fn of(insn: &Insn) -> Option<Access> {
        let Insn::Plain { op, operands } = insn else {
            return None;
        };
        let byte = |at: usize| operands.get(at).copied();
        let short = |at: usize| Some(u16::from_be_bytes([byte(at)?, byte(at + 1)?]));
        Some(match *op {
            0x15..=0x19 => Access::Load {
                kind: op - 0x15,
                slot: u16::from(byte(0)?),
            },
            0x1a..=0x2d => Access::Load {
                kind: (op - 0x1a) / 4,
                slot: u16::from((op - 0x1a) % 4),
            },
            0x36..=0x3a => Access::Store {
                kind: op - 0x36,
                slot: u16::from(byte(0)?),
            },
            0x3b..=0x4e => Access::Store {
                kind: (op - 0x3b) / 4,
                slot: u16::from((op - 0x3b) % 4),
            },
            IINC => Access::Iinc {
                slot: u16::from(byte(0)?),
                by: i16::from(byte(1)? as i8),
            },
            WIDE => match byte(0)? {
                inner @ 0x15..=0x19 => Access::Load {
                    kind: inner - 0x15,
                    slot: short(1)?,
                },
                inner @ 0x36..=0x3a => Access::Store {
                    kind: inner - 0x36,
                    slot: short(1)?,
                },
                IINC => Access::Iinc {
                    slot: short(1)?,
                    by: short(3)? as i16,
                },
                _ => return None,
            },
            _ => return None,
        })
    }

    fn slot(self) -> u16 {
        match self {
            Access::Load { slot, .. } | Access::Store { slot, .. } | Access::Iinc { slot, .. } => {
                slot
            }
        }
    }

    /// The slots this access uses: a `long`/`double` load or store both words, `iinc` its own.
    fn slots(self) -> impl Iterator<Item = u16> {
        let wide = matches!(
            self,
            Access::Load { kind: 1 | 3, .. } | Access::Store { kind: 1 | 3, .. }
        );
        let slot = self.slot();
        std::iter::once(slot).chain(wide.then_some(slot + 1))
    }

    /// This access of `slot`, in the form ASM's writer chooses for it.
    fn encode(self, slot: u16) -> Insn {
        let plain = |op: u8, operands: Vec<u8>| Insn::Plain { op, operands };
        let [high, low] = slot.to_be_bytes();
        match self {
            Access::Load { kind, .. } | Access::Store { kind, .. } => {
                let (base, short_base) = if matches!(self, Access::Load { .. }) {
                    (0x15, 0x1a)
                } else {
                    (0x36, 0x3b)
                };
                match u8::try_from(slot) {
                    Ok(slot @ 0..=3) => plain(short_base + kind * 4 + slot, Vec::new()),
                    Ok(slot) => plain(base + kind, vec![slot]),
                    Err(_) => plain(WIDE, vec![base + kind, high, low]),
                }
            }
            Access::Iinc { by, .. } => match (u8::try_from(slot), i8::try_from(by)) {
                (Ok(slot), Ok(by)) => plain(IINC, vec![slot, by as u8]),
                _ => {
                    let [by_high, by_low] = by.to_be_bytes();
                    plain(WIDE, vec![IINC, high, low, by_high, by_low])
                }
            },
        }
    }
}

/// Every slot `insn` uses (see [`Access::slots`]).
fn touched(insn: &Insn) -> Vec<u16> {
    Access::of(insn).map_or_else(Vec::new, |access| access.slots().collect())
}

/// Where each slot moved, by its old number; `None` for a slot nothing uses.
pub(crate) struct Renumbering {
    moved: Vec<Option<u16>>,
}

impl Renumbering {
    /// The new number of `slot`, which must be a used one.
    pub(crate) fn slot(&self, slot: u16) -> Option<u16> {
        self.moved.get(usize::from(slot)).copied().flatten()
    }

    /// A frame's locals, one entry per old slot, with the unused slots left out.
    pub(crate) fn locals<T>(&self, slots: Vec<T>) -> Vec<T> {
        slots
            .into_iter()
            .enumerate()
            .filter(|(slot, _)| self.moved.get(*slot).copied().flatten().is_some())
            .map(|(_, local)| local)
            .collect()
    }
}

/// Close the gaps among the slots `nodes` and `fixed` (`this`, the parameters and the named locals,
/// both words of a wide one) use, renumbering every access in `nodes`; `None` when there is none.
pub(crate) fn compact(
    nodes: &mut [(Insn, Placement)],
    fixed: &BTreeSet<u16>,
) -> Option<Renumbering> {
    let mut used = fixed.clone();
    for (insn, _) in nodes.iter() {
        used.extend(touched(insn));
    }
    let highest = *used.last()?;
    if used.len() == usize::from(highest) + 1 {
        return None;
    }
    let mut moved = vec![None; usize::from(highest) + 1];
    for (rank, &slot) in used.iter().enumerate() {
        moved[usize::from(slot)] = Some(u16::try_from(rank).ok()?);
    }
    let renumbering = Renumbering { moved };
    for (insn, _) in nodes.iter_mut() {
        let Some(access) = Access::of(insn) else {
            continue;
        };
        let slot = renumbering.slot(access.slot())?;
        if slot != access.slot() {
            *insn = access.encode(slot);
        }
    }
    Some(renumbering)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(op: u8, operands: &[u8]) -> Insn {
        Insn::Plain {
            op,
            operands: operands.to_vec(),
        }
    }

    fn run(insns: &[Insn], fixed: &[u16]) -> Option<Vec<Insn>> {
        let mut nodes: Vec<(Insn, Placement)> = insns
            .iter()
            .enumerate()
            .map(|(index, insn)| (insn.clone(), Placement::Original(index)))
            .collect();
        compact(&mut nodes, &fixed.iter().copied().collect())
            .map(|_| nodes.into_iter().map(|(insn, _)| insn).collect())
    }

    #[test]
    fn a_folded_temporary_leaves_its_slot_to_the_next_local() {
        // `for (e in xs)` after its iterable's temporary (slot 1) was folded: 0 aload_0;
        // 1 invokeinterface iterator; 2 astore_2; 3 aload_2; 4 areturn.
        let insns = [
            op(0x2a, &[]),
            op(0xb9, &[0, 7, 1, 0]),
            op(0x4d, &[]),
            op(0x2c, &[]),
            op(0xb0, &[]),
        ];
        assert_eq!(
            run(&insns, &[0]),
            Some(vec![
                op(0x2a, &[]),
                op(0xb9, &[0, 7, 1, 0]),
                op(0x4c, &[]),
                op(0x2b, &[]),
                op(0xb0, &[]),
            ])
        );
    }

    #[test]
    fn a_method_whose_slots_are_all_used_is_left_alone() {
        // 0 iload_0; 1 istore_1; 2 iinc 1 by 1; 3 iload_1; 4 ireturn.
        let insns = [
            op(0x1a, &[]),
            op(0x3c, &[]),
            op(IINC, &[1, 1]),
            op(0x1b, &[]),
            op(0xac, &[]),
        ];
        assert_eq!(run(&insns, &[0]), None);
    }

    #[test]
    fn a_long_keeps_both_words_and_a_short_form_below_four() {
        // A long at slots 5-6 over a gap at 1-4 moves to 1-2, and `lload 5` becomes `lload_1`.
        let insns = [op(0x37, &[5]), op(0x16, &[5]), op(0xad, &[])];
        assert_eq!(
            run(&insns, &[0]),
            Some(vec![op(0x40, &[]), op(0x1f, &[]), op(0xad, &[])])
        );
    }

    #[test]
    fn a_named_local_keeps_its_slot_used() {
        // Slot 1 is named by the local variable table though no instruction reads it.
        let insns = [op(0x2c, &[]), op(0xb0, &[])];
        assert_eq!(run(&insns, &[0, 1]), None);
    }

    #[test]
    fn a_removed_named_local_releases_its_slot() {
        // DCE removed the LVT entry at slot 1 before compaction. The surviving access at slot 2
        // therefore moves into slot 1 instead of preserving a gap for dead debug metadata.
        let insns = [op(0x2c, &[]), op(0xb0, &[])];
        assert_eq!(run(&insns, &[0]), Some(vec![op(0x2b, &[]), op(0xb0, &[])]));
    }

    #[test]
    fn a_wide_access_narrows_when_its_slot_drops_below_256() {
        let insns = [
            op(WIDE, &[0x3a, 1, 0]),
            op(WIDE, &[IINC, 1, 0, 1, 0]),
            op(WIDE, &[0x19, 1, 0]),
            op(0xb0, &[]),
        ];
        assert_eq!(
            run(&insns, &[0]),
            Some(vec![
                op(0x4c, &[]),
                op(WIDE, &[IINC, 0, 1, 1, 0]),
                op(0x2b, &[]),
                op(0xb0, &[]),
            ])
        );
    }
}
