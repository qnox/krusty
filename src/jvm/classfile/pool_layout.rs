//! The constant pool of a serialized class, laid out again the way kotlinc's writer would have
//! built it.
//!
//! krusty interns constants while it emits a body, but kotlinc (ASM's `ClassWriter`) interns them
//! when the body it has already optimized is written. A body a bytecode rewrite changed therefore
//! owns entries kotlinc never had: an instruction a pass removed leaves its constants behind, and
//! one a pass introduced interned its constants late, behind the method's other entries. Once the
//! class is serialized, the pool is laid out again:
//!
//! - an entry nothing in the class names any more is dropped;
//! - the entries of a rewritten method ([`RelaidMethod`]) that only rewritten code names are
//!   placed where the method's constants began, in the order ASM's `MethodWriter` interns the
//!   rewritten body: catch types, then each instruction's operands, then the local-variable
//!   tables, then the frames' classes, every entry after the entries it names;
//! - every other entry keeps its relative place.
//!
//! Every index in the class is then renumbered. A class whose `ldc` operand would no longer fit in
//! one byte keeps the pool as interned: an instruction's length cannot change after its method's
//! offsets are final. An `ldc_w` whose index comes to fit in one byte stays `ldc_w`, where ASM
//! would write `ldc`.

mod index_slots;

use std::collections::HashMap;
use std::ops::Range;

use index_slots::{ClassSlots, Holder, Part, Unread};

/// A method a bytecode rewrite changed, with the pool entries it interned.
pub(super) struct RelaidMethod {
    /// Position in the class's method table.
    pub(super) index: usize,
    /// The indices interned while the method was emitted and added: those past the previous
    /// method's.
    pub(super) added: Range<u16>,
    /// The indices its rewrite interned when the class was written.
    pub(super) interned: Range<u16>,
}

/// Why a pool is kept as interned.
#[derive(Debug, PartialEq, Eq)]
enum Kept {
    Unread(Unread),
    /// An `ldc` operand whose entry would move past index 255.
    NarrowOperand,
    TooManyEntries,
}

/// `class` with its pool laid out again (see the module documentation).
pub(super) fn relayout(class: Vec<u8>, relaid: &[RelaidMethod]) -> Vec<u8> {
    match relaid_class(&class, relaid) {
        Ok(Some(laid_out)) => laid_out,
        Ok(None) => class,
        Err(kept) => {
            crate::trace_compiler!("bytecode", "pool kept as interned: {kept:?}");
            class
        }
    }
}

/// `class` with its pool laid out again, or `None` when the layout is the one it has.
fn relaid_class(class: &[u8], relaid: &[RelaidMethod]) -> Result<Option<Vec<u8>>, Kept> {
    let read = index_slots::read(class).map_err(Kept::Unread)?;
    let order = Layout::of(&read, relaid).order();
    let unchanged = order
        .iter()
        .map(|&index| usize::from(index))
        .eq((1..read.entries.len()).filter(|&index| read.entries[index].is_some()));
    if unchanged {
        return Ok(None);
    }
    rewrite(class, &read, &order).map(Some)
}

/// What the class names, and where each rewritten method's constants go.
struct Layout<'a> {
    read: &'a ClassSlots,
    /// Entries some index in the class reaches.
    kept: Vec<bool>,
    /// Kept entries that only rewritten code reaches and that a rewritten method interned: they
    /// are placed by the code that names them.
    movable: Vec<bool>,
    /// Per rewritten method: the index its constants are placed before, and the entries its code
    /// names in ASM's order.
    sequences: Vec<(usize, Vec<u16>)>,
}

impl<'a> Layout<'a> {
    fn of(read: &'a ClassSlots, relaid: &[RelaidMethod]) -> Self {
        let count = read.entries.len();
        let mut kept = vec![false; count];
        let mut fixed = vec![false; count];
        let mut owned = vec![false; count];
        let by_method: HashMap<usize, usize> = relaid
            .iter()
            .enumerate()
            .map(|(at, method)| (method.index, at))
            .collect();
        for method in relaid {
            for index in method.added.clone().chain(method.interned.clone()) {
                if let Some(owned) = owned.get_mut(usize::from(index)) {
                    *owned = true;
                }
            }
        }
        let mut parts: Vec<[Vec<u16>; 4]> = relaid.iter().map(|_| Default::default()).collect();
        for slot in &read.slots {
            reach(read, slot.index, &mut kept);
            let rewritten = match slot.holder {
                Holder::Code(method, part) => by_method.get(&method).map(|&at| (at, part)),
                Holder::Class => None,
            };
            match rewritten {
                Some((at, part)) => parts[at][part_order(part)].push(slot.index),
                None => reach(read, slot.index, &mut fixed),
            }
        }
        let movable: Vec<bool> = (0..count)
            .map(|index| kept[index] && !fixed[index] && owned[index])
            .collect();
        let sequences = relaid
            .iter()
            .zip(parts)
            .map(|(method, parts)| {
                let first = method
                    .added
                    .clone()
                    .map(usize::from)
                    .find(|&index| movable.get(index).copied().unwrap_or(false));
                (
                    first.unwrap_or(usize::from(method.added.end)),
                    parts.concat(),
                )
            })
            .collect();
        Layout {
            read,
            kept,
            movable,
            sequences,
        }
    }

    /// The kept entries in their new order.
    fn order(&self) -> Vec<u16> {
        let count = self.read.entries.len();
        let mut placed = vec![false; count];
        let mut order = Vec::new();
        let mut anchors: Vec<&(usize, Vec<u16>)> = self.sequences.iter().collect();
        anchors.sort_by_key(|(anchor, _)| *anchor);
        let mut anchors = anchors.into_iter().peekable();
        for index in 1..count {
            while let Some((_, sequence)) = anchors.next_if(|(anchor, _)| *anchor <= index) {
                for &entry in sequence {
                    self.place(entry, &mut placed, &mut order);
                }
            }
            if self.read.entries[index].is_some() && !self.movable[index] {
                self.place(index as u16, &mut placed, &mut order);
            }
        }
        for (_, sequence) in anchors {
            for &entry in sequence {
                self.place(entry, &mut placed, &mut order);
            }
        }
        // Every kept entry is placed by now: it is named by a slot or by a placed entry. Placing
        // the rest keeps a class whole should that ever not hold.
        for index in 1..count {
            self.place(index as u16, &mut placed, &mut order);
        }
        order
    }

    /// Place `index` after the entries it names, unless it is placed or dropped.
    fn place(&self, index: u16, placed: &mut [bool], order: &mut Vec<u16>) {
        let at = usize::from(index);
        if placed.get(at).copied().unwrap_or(true) || !self.kept[at] {
            return;
        }
        placed[at] = true;
        if let Some(entry) = &self.read.entries[at] {
            for &(_, component) in &entry.components {
                self.place(component, placed, order);
            }
        }
        order.push(index);
    }
}

fn part_order(part: Part) -> usize {
    match part {
        Part::Catch => 0,
        Part::Operand => 1,
        Part::Local => 2,
        Part::Frame => 3,
    }
}

/// Mark `index` and every entry it names as reached.
fn reach(read: &ClassSlots, index: u16, reached: &mut [bool]) {
    let mut pending = vec![index];
    while let Some(index) = pending.pop() {
        let at = usize::from(index);
        if reached.get(at).copied().unwrap_or(true) {
            continue;
        }
        reached[at] = true;
        if let Some(entry) = &read.entries[at] {
            pending.extend(entry.components.iter().map(|&(_, component)| component));
        }
    }
}

/// `class` with the pool entries in `order` and every index renumbered.
fn rewrite(class: &[u8], read: &ClassSlots, order: &[u16]) -> Result<Vec<u8>, Kept> {
    let mut renumbered = vec![0u16; read.entries.len()];
    let mut next: u16 = 1;
    for &index in order {
        renumbered[usize::from(index)] = next;
        let wide = read.entries[usize::from(index)]
            .as_ref()
            .is_some_and(|entry| entry.wide);
        next = next
            .checked_add(if wide { 2 } else { 1 })
            .ok_or(Kept::TooManyEntries)?;
    }
    let mut out = Vec::with_capacity(class.len());
    out.extend_from_slice(&class[..8]);
    out.extend_from_slice(&next.to_be_bytes());
    for &index in order {
        let Some(entry) = &read.entries[usize::from(index)] else {
            continue;
        };
        let start = out.len();
        out.extend_from_slice(&class[entry.range.clone()]);
        for &(offset, component) in &entry.components {
            let component = renumbered[usize::from(component)];
            out[start + offset..start + offset + 2].copy_from_slice(&component.to_be_bytes());
        }
    }
    let shift = out.len() as isize - read.pool_end as isize;
    out.extend_from_slice(&class[read.pool_end..]);
    for slot in &read.slots {
        let index = renumbered[usize::from(slot.index)];
        let at = (slot.at as isize + shift) as usize;
        if slot.narrow {
            out[at] = u8::try_from(index).map_err(|_| Kept::NarrowOperand)?;
        } else {
            out[at..at + 2].copy_from_slice(&index.to_be_bytes());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
