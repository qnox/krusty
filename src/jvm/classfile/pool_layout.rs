//! The constant pool of a serialized class, laid out again the way kotlinc's writer would have
//! built it.
//!
//! krusty interns constants while it emits a body, but kotlinc (ASM's `ClassWriter`) interns them
//! when the body it has already optimized is written. A body a bytecode rewrite changed therefore
//! owns entries kotlinc never had: an instruction a pass removed leaves its constants behind, and
//! one a pass introduced interned its constants late, behind the method's other entries. Once the
//! class is serialized, the pool is laid out again:
//!
//! - an entry nothing in the class names any more is dropped, unless the class is a copy
//!   ([`Unnamed::Kept`]): ASM's writer keeps what each visit of the copied class interned, such as
//!   the original's `EnclosingMethod` a regenerated inline object replaces. Even then, an entry
//!   a rewritten method interned and nothing names after the rewrite goes;
//! - the entries of a rewritten method ([`RelaidMethod`]) that only rewritten code names are
//!   placed where the method's constants began, in the order ASM's `MethodWriter` interns the
//!   rewritten body: catch types, then each instruction's operands, then the local-variable
//!   tables, then the frames' classes, every entry after the entries it names. That includes an
//!   entry something kotlinc visits after the method also names, such as `@Metadata`: the
//!   rewritten code interned it first;
//! - the entries a coroutine transformation interned for the class ahead of its body (a suspend
//!   lambda's spill fields and `@DebugMetadata`) lead that method's entries: kotlinc's
//!   transformer visits them as it writes the method, before the body's code;
//! - an entry a rewritten method interned for code its rewrite removed, and that code emitted
//!   as it was names later, is placed where that code first names it: ASM interns it there;
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
    /// The indices its coroutine transformation named for the class ahead of its body, in the
    /// order it named them.
    pub(super) leading: Vec<u16>,
    /// The indices its rewrite interned when the class was written.
    pub(super) interned: Range<u16>,
}

/// What becomes of an entry nothing in the class names and no rewrite interned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Unnamed {
    /// A class krusty emits: kotlinc's writer interns a constant only when it writes the code or
    /// attribute that names it, so the entry goes.
    Dropped,
    /// A class copied from a compiled one: every visit interned its constants, and they stay.
    Kept,
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
pub(super) fn relayout(class: Vec<u8>, relaid: &[RelaidMethod], unnamed: Unnamed) -> Vec<u8> {
    match relaid_class(&class, relaid, unnamed) {
        Ok(Some(laid_out)) => laid_out,
        Ok(None) => class,
        Err(kept) => {
            crate::trace_compiler!("bytecode", "pool kept as interned: {kept:?}");
            class
        }
    }
}

/// `class` with its pool laid out again, or `None` when the layout is the one it has.
fn relaid_class(
    class: &[u8],
    relaid: &[RelaidMethod],
    unnamed: Unnamed,
) -> Result<Option<Vec<u8>>, Kept> {
    let read = index_slots::read(class).map_err(Kept::Unread)?;
    let order = Layout::of(&read, relaid, unnamed).order();
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
    /// Entries that stay: those some index in the class reaches, in a copied class those no
    /// rewrite interned, and what they name.
    kept: Vec<bool>,
    /// Kept entries that only rewritten code reaches and that a rewritten method interned: they
    /// are placed by the code that names them.
    movable: Vec<bool>,
    /// Per rewritten method: the index its constants are placed before, and the entries its code
    /// names in ASM's order.
    sequences: Vec<(usize, Vec<u16>)>,
    /// Entries a rewrite left only to code emitted as it was, with the index each is placed
    /// before (see [`orphan_anchors`]).
    anchored: Vec<Option<usize>>,
}

impl<'a> Layout<'a> {
    fn of(read: &'a ClassSlots, relaid: &[RelaidMethod], unnamed: Unnamed) -> Self {
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
        let mut rewritten_reach = vec![false; count];
        for slot in &read.slots {
            reach(read, slot.index, &mut kept);
            let rewritten = match slot.holder {
                Holder::Code(method, part) => by_method.get(&method).map(|&at| (at, part)),
                Holder::Class | Holder::AttributeName => None,
            };
            match rewritten {
                Some((at, part)) => {
                    parts[at][part_order(part)].push(slot.index);
                    reach(read, slot.index, &mut rewritten_reach);
                }
                None => reach(read, slot.index, &mut fixed),
            }
        }
        // An entry rewritten code names stays where it is only when kotlinc's writer interns it
        // before that code, for another holder it visits first (the method's header, an earlier
        // method's code). A holder it visits later (a later method, a field, `@Metadata`) finds
        // the entry the code interned.
        let mut visited = vec![false; count];
        let mut visited_first = vec![false; count];
        // Where in kotlinc's visit order each entry is first named, and each method's code begins.
        let mut first_named = vec![usize::MAX; count];
        let mut code_starts: HashMap<usize, usize> = HashMap::new();
        for (position, slot) in read.visit_order().enumerate() {
            if let Holder::Code(method, _) = slot.holder {
                code_starts.entry(method).or_insert(position);
            }
            let rewritten = matches!(
                slot.holder,
                Holder::Code(method, _) if by_method.contains_key(&method)
            );
            for index in reached_from(read, slot.index) {
                if !std::mem::replace(&mut visited[index], true) {
                    first_named[index] = position;
                    if !rewritten {
                        visited_first[index] = true;
                    }
                }
            }
        }
        // A transformation's leading entry moves ahead of its method's code unless kotlinc's writer
        // interns it earlier, for a holder it visits before that code.
        let mut leading = vec![false; count];
        for method in relaid {
            let code_start = code_starts
                .get(&method.index)
                .copied()
                .unwrap_or(usize::MAX);
            for &index in &method.leading {
                let at = usize::from(index);
                if at < count && first_named[at] >= code_start {
                    leading[at] = true;
                }
            }
        }
        for index in 0..count {
            if rewritten_reach[index] {
                fixed[index] = visited_first[index];
            }
        }
        let orphaned: Vec<bool> = (0..count)
            .map(|index| owned[index] && kept[index] && !rewritten_reach[index] && !leading[index])
            .collect();
        let anchored = orphan_anchors(read, &by_method, &orphaned);
        if unnamed == Unnamed::Kept {
            let unowned: Vec<u16> = (1..count)
                .filter(|&index| read.entries[index].is_some() && !owned[index])
                .map(|index| index as u16)
                .collect();
            for index in unowned {
                reach(read, index, &mut kept);
            }
        }
        let movable: Vec<bool> = (0..count)
            .map(|index| kept[index] && (leading[index] || !fixed[index] && owned[index]))
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
                let leading = method
                    .leading
                    .iter()
                    .copied()
                    .filter(|&index| leading.get(usize::from(index)).copied().unwrap_or(false));
                (
                    first.unwrap_or(usize::from(method.added.end)),
                    leading.chain(parts.concat()).collect(),
                )
            })
            .collect();
        Layout {
            read,
            kept,
            movable,
            sequences,
            anchored,
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
        let mut orphans: Vec<(usize, u16)> = (0..count)
            .filter_map(|index| Some((self.anchored[index]?, index as u16)))
            .collect();
        orphans.sort_unstable();
        let mut orphans = orphans.into_iter().peekable();
        // A sequence goes before an orphan with the same anchor; an anchor past the last entry
        // places its sequence or orphan at the end, still in anchor order.
        for index in 1..=count {
            let bound = if index == count { usize::MAX } else { index };
            loop {
                let sequence = anchors.peek().map(|(anchor, _)| *anchor);
                let orphan = orphans.peek().map(|&(anchor, _)| anchor);
                match (sequence, orphan) {
                    (Some(sequence), orphan)
                        if sequence <= bound && orphan.is_none_or(|orphan| sequence <= orphan) =>
                    {
                        let (_, sequence) = anchors.next().expect("a peeked sequence");
                        for &entry in sequence {
                            self.place(entry, &mut placed, &mut order);
                        }
                    }
                    (_, Some(orphan)) if orphan <= bound => {
                        let (_, orphan) = orphans.next().expect("a peeked orphan");
                        self.place(orphan, &mut placed, &mut order);
                    }
                    _ => break,
                }
            }
            if index < count
                && self.read.entries[index].is_some()
                && !self.movable[index]
                && self.anchored[index].is_none()
            {
                self.place(index as u16, &mut placed, &mut order);
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
            for component in entry.named() {
                self.place(component, placed, order);
            }
        }
        order.push(index);
    }
}

/// Where each `orphaned` entry goes: an entry a rewritten method interned that its rewritten
/// code no longer names, first named (in kotlinc's visit order) by the code of a method emitted
/// as it was, is placed before the first entry that code names for the first time with or after
/// it, or right after the last entry that method names when none follows. An entry some other
/// slot names first stays where it is.
fn orphan_anchors(
    read: &ClassSlots,
    by_method: &HashMap<usize, usize>,
    orphaned: &[bool],
) -> Vec<Option<usize>> {
    let count = read.entries.len();
    let mut anchored = vec![None; count];
    if !orphaned.contains(&true) {
        return anchored;
    }
    let mut seen = vec![false; count];
    let mut method = None;
    // The last entry the current method names: its header, then its code.
    let mut named_max = 0;
    // The last entry the slots since the last code slot name: the next method's header.
    let mut header_max = 0;
    let mut pending: Vec<usize> = Vec::new();
    let settle = |pending: &mut Vec<usize>, anchor: usize, anchored: &mut [Option<usize>]| {
        for orphan in pending.drain(..) {
            if anchor > orphan {
                anchored[orphan] = Some(anchor);
            }
        }
    };
    for slot in read.visit_order() {
        let code = match slot.holder {
            Holder::Code(at, _) if !by_method.contains_key(&at) => Some(at),
            _ => None,
        };
        let reached = reached_from(read, slot.index);
        let Some(at) = code else {
            for &index in &reached {
                seen[index] = true;
                if !orphaned[index] && slot.holder == Holder::Class {
                    header_max = header_max.max(index);
                }
            }
            if matches!(slot.holder, Holder::Code(..)) {
                header_max = 0;
            }
            continue;
        };
        if method != Some(at) {
            settle(&mut pending, named_max + 1, &mut anchored);
            method = Some(at);
            named_max = header_max;
        }
        header_max = 0;
        for &index in &reached {
            if orphaned[index] && !seen[index] {
                pending.push(index);
            }
        }
        let fresh = reached
            .iter()
            .copied()
            .filter(|&index| !seen[index] && !orphaned[index])
            .min();
        if let Some(fresh) = fresh {
            settle(&mut pending, fresh, &mut anchored);
        }
        for index in reached {
            seen[index] = true;
            if !orphaned[index] {
                named_max = named_max.max(index);
            }
        }
    }
    settle(&mut pending, named_max + 1, &mut anchored);
    anchored
}

/// `index` and every entry it names, each once.
fn reached_from(read: &ClassSlots, index: u16) -> Vec<usize> {
    let mut reached = Vec::new();
    let mut pending = vec![index];
    while let Some(index) = pending.pop() {
        let at = usize::from(index);
        if at >= read.entries.len() || reached.contains(&at) {
            continue;
        }
        reached.push(at);
        if let Some(entry) = &read.entries[at] {
            pending.extend(entry.named());
        }
    }
    reached
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
            pending.extend(entry.named());
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
