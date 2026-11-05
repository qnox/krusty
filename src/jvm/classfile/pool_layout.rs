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
//! - an emitted call-site copy can retain only the declaration-header roots its transformation
//!   visited before replacing them, without keeping other unnamed entries;
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
//! kotlinc lists `InnerClasses` rows whose qualified names tie in the order its pool met their
//! classes. The rows the writer passes as tied are therefore ordered by their classes' places in
//! the pool laid out here.
//!
//! Every index in the class is then renumbered, and each `ldc` takes the width ASM would write for
//! its renumbered entry (see [`ldc_width`]).

mod index_slots;
mod ldc_width;

use crate::fx_hash::HashMap;
use std::ops::Range;

use super::inner_classes::TiedInnerRows;
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
    /// A method whose `ldc` widths cannot be changed.
    Unresized(ldc_width::Unresized),
    TooManyEntries,
}

/// `class` with its pool laid out again (see the module documentation).
pub(super) fn relayout(
    class: Vec<u8>,
    relaid: &[RelaidMethod],
    unnamed: Unnamed,
    retained: &[u16],
    tied_rows: &[TiedInnerRows],
) -> Vec<u8> {
    match relaid_class(&class, relaid, unnamed, retained, tied_rows) {
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
    retained: &[u16],
    tied_rows: &[TiedInnerRows],
) -> Result<Option<Vec<u8>>, Kept> {
    let read = index_slots::read(class).map_err(Kept::Unread)?;
    let order = Layout::of(&read, relaid, unnamed, retained).order();
    let unchanged = order
        .iter()
        .map(|&index| usize::from(index))
        .eq((1..read.entries.len()).filter(|&index| read.entries[index].is_some()));
    if unchanged {
        return Ok(None);
    }
    rewrite(class, &read, &order, tied_rows).map(Some)
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
    /// Entries a rewrite left only to code emitted as it was, with where each is placed (see
    /// [`orphan_anchors`]).
    anchored: Vec<Option<Anchor>>,
}

/// Where an orphaned entry is placed: before the entry at `before`, after the orphans anchored
/// there that kotlinc's visit reached earlier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Anchor {
    before: usize,
    /// The entry's rank in visit order among the orphans.
    visit: usize,
}

impl<'a> Layout<'a> {
    fn of(
        read: &'a ClassSlots,
        relaid: &[RelaidMethod],
        unnamed: Unnamed,
        retained: &[u16],
    ) -> Self {
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
                Holder::Class
                | Holder::AttributeName
                | Holder::CodeAttribute(_)
                | Holder::Parameter(_) => None,
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
        let code_starts = method_code_starts(read);
        let mut named = Reach::new(read);
        for (position, slot) in read.visit_order().enumerate() {
            let rewritten = matches!(
                slot.holder,
                Holder::Code(method, _) if by_method.contains_key(&method)
            );
            for &index in named.from(read, slot.index) {
                if !std::mem::replace(&mut visited[index], true) {
                    first_named[index] = position;
                    if !rewritten {
                        visited_first[index] = true;
                    }
                }
            }
        }
        // A transformation's leading entry moves ahead of its method's replayed node (its
        // `MethodParameters` names, then its code) unless kotlinc's writer interns it earlier, for
        // a holder it visits before that node.
        let mut leading = vec![false; count];
        let replay_starts = method_replay_starts(read, &code_starts);
        for method in relaid {
            let code_start = replay_starts
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
            .map(|index| {
                owned[index]
                    && kept[index]
                    && !rewritten_reach[index]
                    && !leading[index]
                    // A class signature is visited with the class header by ASM. It may fall into
                    // the first method's coarse pool range, but that method never owns it.
                    && !read.early_class_entries.contains(&(index as u16))
            })
            .collect();
        let anchored = orphan_anchors(read, relaid, &by_method, &orphaned);
        if unnamed == Unnamed::Kept {
            let unowned: Vec<u16> = (1..count)
                .filter(|&index| read.entries[index].is_some() && !owned[index])
                .map(|index| index as u16)
                .collect();
            for index in unowned {
                reach(read, index, &mut kept);
            }
        }
        // A copied declaration header can be visited and then replaced while ASM reuses the same
        // ClassWriter. Keep only the roots that visit interned, plus the entries they name; unlike
        // `Unnamed::Kept`, this does not retain unrelated constants from an emitted class.
        for &index in retained {
            reach(read, index, &mut kept);
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
        let orphans: Vec<(usize, u16)> = {
            let mut anchored: Vec<(Anchor, u16)> = (0..count)
                .filter_map(|index| Some((self.anchored[index]?, index as u16)))
                .collect();
            anchored.sort_unstable();
            anchored
                .into_iter()
                .map(|(anchor, index)| (anchor.before, index))
                .collect()
        };
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

/// Where each method's code begins in kotlinc's visit order.
///
/// A body operand is that boundary. An empty body has none, so its `Code` attribute name stands in.
/// The attribute name of a method that does have a body sits before the method's own attributes, and
/// treating it as the boundary moves a parameter annotation — interned ahead of the body — to the
/// attribute's name.
fn method_code_starts(read: &ClassSlots) -> HashMap<usize, usize> {
    let mut body = HashMap::default();
    let mut attribute = HashMap::default();
    for (position, slot) in read.visit_order().enumerate() {
        match slot.holder {
            Holder::Code(method, _) => {
                body.entry(method).or_insert(position);
            }
            Holder::CodeAttribute(method) => {
                attribute.entry(method).or_insert(position);
            }
            Holder::Class | Holder::AttributeName | Holder::Parameter(_) => {}
        }
    }
    for (method, position) in attribute {
        body.entry(method).or_insert(position);
    }
    body
}

/// Where each method's node is replayed into kotlinc's writer: its first `MethodParameters` name,
/// else its code.
fn method_replay_starts(
    read: &ClassSlots,
    code_starts: &HashMap<usize, usize>,
) -> HashMap<usize, usize> {
    let mut starts = code_starts.clone();
    for (position, slot) in read.visit_order().enumerate() {
        if let Holder::Parameter(method) = slot.holder {
            let start = starts.entry(method).or_insert(position);
            *start = (*start).min(position);
        }
    }
    starts
}

/// Where each `orphaned` entry goes: an entry a rewritten method interned that its rewritten
/// code no longer names, first named (in kotlinc's visit order) by later code, a later header, or
/// a class attribute, is placed before the first entry that visit names for the first time with or
/// after it. When none follows, it goes right after the last entry that visit names: the method's
/// last entry for its code, the header's last entry for a method header.
fn orphan_anchors(
    read: &ClassSlots,
    relaid: &[RelaidMethod],
    by_method: &HashMap<usize, usize>,
    orphaned: &[bool],
) -> Vec<Option<Anchor>> {
    let count = read.entries.len();
    let mut anchored = vec![None; count];
    if !orphaned.contains(&true) {
        return anchored;
    }
    let mut seen = vec![false; count];
    let mut owner = vec![None; count];
    for method in relaid {
        for index in method.added.clone().chain(method.interned.clone()) {
            if let Some(entry_owner) = owner.get_mut(usize::from(index)) {
                *entry_owner = Some(method.index);
            }
        }
    }
    let code_starts = method_code_starts(read);
    let mut method = None;
    // The last entry the current method names: its header, then its code.
    let mut named_max = 0;
    // The last entry the slots since the last code slot name: the next method's header.
    let mut header_max = 0;
    let mut pending: Vec<usize> = Vec::new();
    // Whether the previous slot was code that is not rewritten.
    let mut in_code = false;
    // Orphans with one anchor keep the order the visit reached them in: a method header's
    // descriptor before the entries its code names.
    let mut visits = 0..;
    let mut settle = |pending: &mut Vec<usize>, before: usize, anchored: &mut [Option<Anchor>]| {
        for orphan in pending.drain(..) {
            if before > orphan {
                let visit = visits.next().expect("an unbounded range");
                anchored[orphan] = Some(Anchor { before, visit });
            }
        }
    };
    let mut named = Reach::new(read);
    for (position, slot) in read.visit_order().enumerate() {
        let code = match slot.holder {
            Holder::Code(at, _) if !by_method.contains_key(&at) => Some(at),
            _ => None,
        };
        let reached = named.from(read, slot.index);
        let Some(at) = code else {
            // ASM interns a method's code entries as it writes that code, so an orphan the code
            // names first is placed right after the last entry the method names, whatever is
            // visited next.
            if std::mem::take(&mut in_code) {
                settle(&mut pending, named_max + 1, &mut anchored);
            }
            // A rewrite-owned entry whose first remaining use comes after that method's code is
            // interned at that later visit, not at the position of the removed instruction. This
            // includes an `InnerClasses` row retaining a holder class that CapturedVars removed.
            for &index in reached {
                if orphaned[index]
                    && !seen[index]
                    && owner[index]
                        .and_then(|method| code_starts.get(&method).copied())
                        .is_some_and(|code_start| position > code_start)
                {
                    pending.push(index);
                } else if !seen[index] && !orphaned[index] {
                    // `reached` follows ASM's component interning order. Settle at the next
                    // freshly interned component, not the numerically smallest old pool index:
                    // an orphaned descriptor in a later NameAndType belongs after its fresh name.
                    settle(&mut pending, index, &mut anchored);
                }
                seen[index] = true;
                if !orphaned[index] && matches!(slot.holder, Holder::Class | Holder::Parameter(_)) {
                    header_max = header_max.max(index);
                }
            }
            if matches!(slot.holder, Holder::Code(..)) {
                header_max = 0;
            }
            continue;
        };
        if method != Some(at) {
            // What is pending now the header named: it follows the header's own entries.
            settle(&mut pending, header_max + 1, &mut anchored);
            method = Some(at);
            named_max = header_max;
        }
        in_code = true;
        header_max = 0;
        for &index in reached {
            if orphaned[index] && !seen[index] {
                pending.push(index);
            } else if !seen[index] && !orphaned[index] {
                settle(&mut pending, index, &mut anchored);
            }
            seen[index] = true;
            if !orphaned[index] {
                named_max = named_max.max(index);
            }
        }
    }
    settle(&mut pending, named_max + 1, &mut anchored);
    anchored
}

/// Scratch for the entries one query names. Queries run once per slot of a class, so the entries a
/// query has seen are stamped with its number instead of cleared or allocated per query.
struct Reach {
    seen: Vec<u32>,
    query: u32,
    reached: Vec<usize>,
}

impl Reach {
    fn new(read: &ClassSlots) -> Self {
        Reach {
            seen: vec![0; read.entries.len()],
            query: 0,
            reached: Vec::new(),
        }
    }

    /// `index` and every entry it names, each once.
    fn from(&mut self, read: &ClassSlots, index: u16) -> &[usize] {
        fn visit(
            read: &ClassSlots,
            index: u16,
            query: u32,
            seen: &mut [u32],
            reached: &mut Vec<usize>,
        ) {
            let at = usize::from(index);
            match seen.get_mut(at) {
                Some(stamp) if *stamp != query => *stamp = query,
                _ => return,
            }
            if let Some(entry) = &read.entries[at] {
                for component in entry.named() {
                    visit(read, component, query, seen, reached);
                }
            }
            reached.push(at);
        }
        self.query += 1;
        self.reached.clear();
        visit(read, index, self.query, &mut self.seen, &mut self.reached);
        &self.reached
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
            pending.extend(entry.named());
        }
    }
}

/// `class` with the pool entries in `order` and every index renumbered, each group of
/// `tied_rows` (the classes of `InnerClasses` rows, by their indices before) in pool order.
fn rewrite(
    class: &[u8],
    read: &ClassSlots,
    order: &[u16],
    tied_rows: &[TiedInnerRows],
) -> Result<Vec<u8>, Kept> {
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
    let pool_end = out.len();
    let shift = pool_end as isize - read.pool_end as isize;
    out.extend_from_slice(&class[read.pool_end..]);
    // The loads whose width the renumbered entry changes: ASM writes `ldc` exactly when the index
    // fits in one byte.
    let mut loads = Vec::new();
    for slot in &read.slots {
        let index = renumbered[usize::from(slot.index)];
        let at = (slot.at as isize + shift) as usize;
        let fits = index <= 0xff;
        if slot.narrow {
            match u8::try_from(index) {
                Ok(index) => out[at] = index,
                Err(_) => loads.push(ldc_width::Load { at, index }),
            }
        } else {
            out[at..at + 2].copy_from_slice(&index.to_be_bytes());
            let ldc_w =
                matches!(slot.holder, Holder::Code(_, Part::Operand)) && out[at - 1] == LDC_W;
            if ldc_w && fits {
                loads.push(ldc_width::Load { at, index });
            }
        }
    }
    if let Some(rows) = read.inner_class_rows {
        let rows = (rows as isize + shift) as usize;
        for tied in tied_rows {
            let renumber = |indices: &[u16]| {
                indices
                    .iter()
                    .map(|&index| renumbered[usize::from(index)])
                    .collect()
            };
            let tied = TiedInnerRows {
                rows: renumber(&tied.rows),
                declarations: renumber(&tied.declarations),
            };
            order_tied_rows(&mut out[rows..], &tied);
        }
    }
    if loads.is_empty() {
        return Ok(out);
    }
    loads.sort_by_key(|load| load.at);
    ldc_width::resized(&out, pool_end, &loads).map_err(Kept::Unresized)
}

const LDC_W: u8 = 0x13;

/// Order the rows of the `InnerClasses` table starting at `table` (its row count) whose classes
/// are `tied` by their classes' pool indices, each row keeping one of the places they held.
fn order_tied_rows(table: &mut [u8], tied: &TiedInnerRows) {
    const ROW: usize = 8;
    let count = usize::from(u16::from_be_bytes([table[0], table[1]]));
    let inner = |row: &[u8]| u16::from_be_bytes([row[0], row[1]]);
    let rows = &mut table[2..2 + count * ROW];
    let places: Vec<usize> = rows
        .chunks(ROW)
        .enumerate()
        .filter(|(_, row)| tied.rows.contains(&inner(row)))
        .map(|(place, _)| place)
        .collect();
    let mut tied_rows: Vec<[u8; ROW]> = places
        .iter()
        .map(|&place| {
            rows[place * ROW..][..ROW]
                .try_into()
                .expect("a row is eight bytes")
        })
        .collect();
    tied_rows.sort_by_key(|row| inner(row));
    let declaration_places = tied_rows
        .iter()
        .enumerate()
        .filter_map(|(place, row)| tied.declarations.contains(&inner(row)).then_some(place))
        .collect::<Vec<_>>();
    let mut declarations = declaration_places
        .iter()
        .map(|&place| tied_rows[place])
        .collect::<Vec<_>>();
    declarations.sort_by_key(|row| {
        tied.declarations
            .iter()
            .position(|&index| index == inner(row))
            .expect("a declared row")
    });
    for (place, row) in declaration_places.into_iter().zip(declarations) {
        tied_rows[place] = row;
    }
    for (place, row) in places.into_iter().zip(tied_rows) {
        rows[place * ROW..][..ROW].copy_from_slice(&row);
    }
}

#[cfg(test)]
mod tests;
