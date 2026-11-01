//! `LineNumberTable` marking.
//!
//! One method's source positions are recorded as (pc, line) pairs while its bytecode is built. The
//! rules about which mark wins at a given offset live here, beside the table they produce, rather
//! than at the emission sites that call them.

use super::{ClassWriter, CodeBuilder, LvtEntry};

/// What a method's next line mark owes: marks already written live in `CodeBuilder::line_marks`.
#[derive(Clone, Default)]
pub(super) struct PendingLines {
    /// The next line mark is written even if its line is the one already in effect: an inlined
    /// call's own lines ended, so the caller's line must be stated again (see
    /// [`CodeBuilder::forget_line`]).
    forgotten: bool,
    /// The pc of a line mark that must own an instruction (see [`CodeBuilder::mark_line_occupied`]).
    occupied_pc: Option<u16>,
    /// The pc of an entry written although its line was already in effect: a forgotten line's
    /// restatement, a control-flow entry, or an inlined body's copied line. A later mark that
    /// replaces it keeps its own entry even when it names the line in effect before it.
    restated_pc: Option<u16>,
    /// The entry at this pc is a statement's eager boundary that kotlinc marks only after the
    /// statement's value: the pc, the entry's index, and the value's token (see
    /// [`CodeBuilder::mark_value_boundary`]).
    value_boundary: Option<(u16, usize, u32)>,
}

/// A call-site line withdrawn by [`CodeBuilder::defer_line_mark`].
pub(crate) struct DeferredLine {
    /// Where the entry stood among the method's entries, and where it goes back.
    index: usize,
    pc: u16,
    line: u16,
    /// The line in effect had been forgotten before the call marked its own, so the mark was due
    /// even for that same line.
    forgotten: bool,
}

impl ClassWriter {
    /// Position of the implicit void return recorded by declared-function emission. `None` for an
    /// abstract, diverging, value-returning, or otherwise non-fallthrough method.
    pub fn method_implicit_void_return_pc(&self, name: &str, desc: &str) -> Option<u16> {
        let (n, d) = (self.cp.lookup_utf8(name)?, self.cp.lookup_utf8(desc)?);
        self.methods
            .iter()
            .find(|method| method.name == n && method.desc == d)?
            .implicit_void_return_pc
    }

    /// Attach kotlinc-style debug tables to a previously-added method (matched by name+descriptor):
    /// a `LineNumberTable` plus a whole-method `LocalVariableTable`. Interns each local's name and
    /// descriptor here so per-method visitation fixes constant-pool order.
    pub fn set_method_debug(
        &mut self,
        name: &str,
        desc: &str,
        lnt: Option<(u16, u32)>,
        locals: &[(String, String, u16)],
    ) {
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        if !self
            .methods
            .iter()
            .any(|method| method.name == n && method.desc == d && method.code.is_some())
        {
            return;
        }
        // A body that marked lines keeps exactly those: kotlinc numbers a method only where its
        // codegen marks a node, never with the declaration's line in front of the first mark.
        let (needs_lnt, needs_lvt) = match self
            .methods
            .iter()
            .find(|method| method.name == n && method.desc == d)
        {
            Some(method) => (method.lnt.is_empty(), method.lvt.is_empty()),
            None => return,
        };
        if !needs_lnt && !needs_lvt {
            return;
        }
        let lvt: Vec<LvtEntry> = if needs_lvt {
            locals
                .iter()
                .map(|(name, descriptor, slot)| {
                    (
                        self.cp.utf8(name),
                        self.cp.utf8(descriptor),
                        *slot,
                        None,
                        None,
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        let latest = self
            .methods
            .last()
            .is_some_and(|method| method.name == n && method.desc == d);
        if needs_lvt && latest {
            // The local names belong to the method just added, as kotlinc's writer interns them
            // with its body. A later method's rewrite must not lay its own entries out ahead of
            // them: a top-level setter's `<set-?>` stays between the setter and the next method.
            self.include_latest_method_pool();
        }
        if let Some(method) = self
            .methods
            .iter_mut()
            .find(|method| method.name == n && method.desc == d)
        {
            if needs_lnt {
                method.lnt = lnt
                    .map(|(pc, line)| (pc, line as u16))
                    .into_iter()
                    .collect();
            }
            if needs_lvt {
                method.lvt = lvt;
            }
        }
    }

    /// Intern an `init` block's local names before the constructor's parameter rows, so a kept
    /// local's strings precede `this` the way kotlinc visits them.
    pub fn reserve_ranged_local_names(&mut self, locals: &[(u16, u16, u16, String, String)]) {
        for (_, _, _, name, descriptor) in locals {
            self.cp.utf8(name);
            self.cp.utf8(descriptor);
        }
    }

    /// Insert constructor-body locals ahead of the parameter rows [`Self::set_method_debug`] wrote.
    ///
    /// kotlinc visits an `init` block's locals before `this` and the constructor parameters. A
    /// zero-length range stays until the method is written: the store in front of it belongs to
    /// that local, and an unused local then disappears from the table.
    pub fn prepend_ranged_locals(
        &mut self,
        name: &str,
        desc: &str,
        locals: &[(u16, u16, u16, String, String)],
    ) {
        if locals.is_empty() {
            return;
        }
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        let ranged = locals
            .iter()
            .map(|(start, length, slot, local_name, local_desc)| {
                (
                    self.cp.utf8(local_name),
                    self.cp.utf8(local_desc),
                    *slot,
                    Some(*start),
                    Some(*length),
                )
            })
            .collect::<Vec<_>>();
        if let Some(method) = self
            .methods
            .iter_mut()
            .find(|method| method.name == n && method.desc == d && method.code.is_some())
        {
            let mut combined = ranged;
            combined.append(&mut method.lvt);
            method.lvt = combined;
        }
    }

    /// Replace a method's `LineNumberTable` with exact `(start_pc, line)` entries. Lookup-only:
    /// describing a missing method does not perturb the constant pool.
    pub fn set_method_lines(&mut self, name: &str, desc: &str, entries: &[(u16, u32)]) {
        let (Some(n), Some(d)) = (self.cp.lookup_utf8(name), self.cp.lookup_utf8(desc)) else {
            return;
        };
        let lineless = |(name_, desc_): &(String, String)| name_ == name && desc_ == desc;
        if self.lineless_methods.iter().any(lineless) {
            return;
        }
        if let Some(method) = self
            .methods
            .iter_mut()
            .find(|method| method.name == n && method.desc == d && method.code.is_some())
        {
            method.lnt = entries
                .iter()
                .map(|&(pc, line)| (pc, line as u16))
                .collect();
        }
    }

    /// Write no `LineNumberTable` for `name desc`, whatever lines its emission collects: kotlinc
    /// writes none for a `<clinit>` that fills `$$delegatedProperties`.
    pub(in crate::jvm) fn omit_method_lines(&mut self, name: &str, desc: &str) {
        self.lineless_methods
            .push((name.to_string(), desc.to_string()));
    }
}

impl CodeBuilder {
    /// Record a `LineNumberTable` entry for `line` starting at the CURRENT pc. Deduped: a re-mark
    /// of the line already in effect is dropped; a second mark at the same pc overwrites (the
    /// statement that actually begins an instruction wins, matching kotlinc's per-statement entries).
    pub fn mark_line(&mut self, line: u32) {
        let _ = self.record_line(line);
    }

    /// Record a source line at a control-flow entry even when the same line was in effect on the
    /// preceding linear path. Exception handlers start a distinct execution path, and kotlinc
    /// retains their entry in the line table rather than deduplicating it against unreachable
    /// fall-through state.
    pub(crate) fn mark_control_entry_line(&mut self, line: u32) {
        if self.dead || self.bytes.len() > u16::MAX as usize {
            return;
        }
        let line = line.min(u16::MAX as u32) as u16;
        let pc = self.bytes.len() as u16;
        match self.line_marks.last_mut() {
            Some((last_pc, last_line)) if *last_pc == pc => *last_line = line,
            _ => self.line_marks.push((pc, line)),
        }
        self.pending_lines.restated_pc = Some(pc);
    }

    /// [`Self::mark_line`], reporting the index of the entry left in effect AT THE CURRENT pc —
    /// `None` when this call wrote none there.
    ///
    /// The answer is what retention is allowed to name. A mark can decline to write at this offset
    /// for three reasons — dead code, a pc past the classfile range, and a dedupe against the line
    /// already in effect at an EARLIER pc — and in each the current offset holds no entry of this
    /// mark's.
    /// Mark `line` as a line that owns code of its own even when it emits none: kotlinc's `init {`
    /// and a block's closing `}`. When the next mark names another line at this same pc, a `nop`
    /// goes first so this line keeps an entry; an instruction emitted in between makes it moot.
    pub fn mark_line_occupied(&mut self, line: u32) {
        if self.record_line(line).is_some() {
            self.pending_lines.occupied_pc = Some(self.bytes.len() as u16);
        }
    }

    fn record_line(&mut self, line: u32) -> Option<usize> {
        if let Some(occupied) = self.pending_lines.occupied_pc.take() {
            let displaces = self
                .line_marks
                .last()
                .is_some_and(|&(pc, current)| pc == occupied && u32::from(current) != line);
            if occupied as usize == self.bytes.len() && displaces && !self.dead {
                self.nop();
            }
        }
        if self.dead {
            return None; // the statement it would mark is dropped dead code (see `dead`)
        }
        if self.bytes.len() > u16::MAX as usize {
            return None; // past the classfile pc range — an entry would silently wrap
        }
        let line = line.min(u16::MAX as u32) as u16;
        let pc = self.bytes.len() as u16;
        let forgotten = std::mem::take(&mut self.pending_lines.forgotten);
        let retained = self.retained_line_mark.is_some_and(|index| {
            // Retention names an ENTRY, so it applies only while that entry is still the last one
            // and still sits at this offset. It therefore expires on its own as soon as the pc
            // advances, and cannot survive as a flag on an offset nothing occupies.
            index + 1 == self.line_marks.len()
                && self
                    .line_marks
                    .get(index)
                    .is_some_and(|(lpc, _)| *lpc == pc)
        });
        match self.line_marks.last() {
            // A retained entry sits here: this mark goes AFTER it, so both survive, even when it
            // names the same line. kotlinc's `visitSetValue` marks an unsigned value and the
            // inlined operation marks that line again before the receiver load. The retention is
            // spent — a third mark at the same offset replaces this one as usual.
            // A kept entry is already this mark when it names the same line, and stays kept for
            // the next mark.
            Some((lpc, ll))
                if *lpc == pc && retained && self.retained_line_distinct && *ll == line =>
            {
                Some(self.line_marks.len() - 1)
            }
            Some((lpc, _)) if *lpc == pc && retained => {
                self.retained_line_mark = None;
                self.line_marks.push((pc, line));
                Some(self.line_marks.len() - 1)
            }
            // Already exactly this entry.
            Some((lpc, ll)) if *lpc == pc && *ll == line => Some(self.line_marks.len() - 1),
            // A replacement that names the line the previous entry already put in effect leaves
            // no entry here at all, as if the replaced mark had never been made: the line in effect
            // is still that line. `boxed(\n).toString()` on the null path of a one-line elvis
            // marks the call's line 7 and then its receiver's line 6 where `boxed()` begins, and
            // kotlinc writes no entry there.
            Some((lpc, _)) if *lpc == pc => {
                let restated = forgotten || self.pending_lines.restated_pc == Some(pc);
                let in_effect = self
                    .line_marks
                    .len()
                    .checked_sub(2)
                    .map(|index| self.line_marks[index].1);
                if !restated && in_effect == Some(line) {
                    self.line_marks.pop();
                    return None;
                }
                if forgotten {
                    self.pending_lines.restated_pc = Some(pc);
                }
                let last = self.line_marks.last_mut()?;
                last.1 = line;
                Some(self.line_marks.len() - 1)
            }
            // The line already in effect, carried from an EARLIER pc: no entry is written here, so
            // this offset holds none of this mark's to retain.
            Some((_, ll)) if *ll == line && !forgotten => None,
            _ => {
                if forgotten {
                    self.pending_lines.restated_pc = Some(pc);
                    // The caller's line written again after an inlined body is that body's
                    // restoration, which a debug-table curation keeps beside an equal entry.
                    if self
                        .line_marks
                        .last()
                        .is_some_and(|&(_, previous)| previous == line)
                    {
                        self.inlined_line_marks.push((pc, line));
                    }
                }
                self.line_marks.push((pc, line));
                Some(self.line_marks.len() - 1)
            }
        }
    }

    /// Record `line` at an offset BEHIND the current one — a spliced body's own line marks, whose
    /// positions the splice fixes and which are added once its bytes are already in place.
    ///
    /// The table must stay ascending by pc, so a mark that would land before the last one already
    /// recorded is refused rather than silently reordering it.
    pub fn add_line_mark_at(&mut self, pc: u16, line: u16) {
        // A `LineNumberTable` `start_pc` must index the code array (JVMS 4.7.12). A spliced body
        // whose bytes were dropped as unreachable leaves its recorded positions past the end, and an
        // entry there makes the whole class unloadable.
        if self.bytes.len() > u16::MAX as usize || pc as usize >= self.bytes.len() {
            return;
        }
        match self.line_marks.last() {
            Some((last_pc, _)) if *last_pc > pc => {}
            Some((last_pc, last_line)) if *last_pc == pc => {
                if *last_line != line {
                    let entry = self.line_marks.last_mut().expect("just observed");
                    entry.1 = line;
                }
            }
            // The line already in effect needs no entry of its own.
            Some((_, last_line)) if *last_line == line => {}
            _ => self.line_marks.push((pc, line)),
        }
    }

    /// Withdraw a line marked at the current offset, which no instruction occupies yet: the code
    /// about to begin there has no source position, as kotlinc builds an implicit context argument,
    /// so the line in effect stays the earlier one until something that has a position marks it.
    /// A retained entry is kept, since it records an inline call site rather than a start.
    pub(crate) fn withdraw_line(&mut self) {
        let pc = self.bytes.len();
        let retained = self.retained_line_mark == Some(self.line_marks.len().wrapping_sub(1));
        if !retained
            && self
                .line_marks
                .last()
                .is_some_and(|(lpc, _)| *lpc as usize == pc)
        {
            self.line_marks.pop();
        }
    }

    /// The current offset begins a statement whose value, named by the opaque token `value`, comes
    /// next and that kotlinc marks only once that value is on the stack, or not at all apart from
    /// it: a declaration (`visitVariable`), a `return` (`visitReturn`), or a block statement
    /// (`visitStatementContainer`). The entries at this offset, the
    /// statement's own and those its value marks eagerly where it starts, are not kotlinc's yet.
    pub(crate) fn mark_value_boundary(&mut self, value: u32) {
        let Ok(pc) = u16::try_from(self.bytes.len()) else {
            return;
        };
        let first = self
            .line_marks
            .iter()
            .rposition(|&(marked, _)| marked != pc)
            .map_or(0, |before| before + 1);
        self.pending_lines.value_boundary = Some((pc, first, value));
    }

    /// The first entry at the current offset and the token of the statement value that
    /// [`Self::mark_value_boundary`] recorded here, while no instruction has been written since.
    pub(crate) fn value_boundary_here(&self) -> Option<(usize, u32)> {
        let (pc, index, value) = self.pending_lines.value_boundary?;
        (usize::from(pc) == self.bytes.len() && index <= self.line_marks.len())
            .then_some((index, value))
    }

    /// Withdraw the call-site line in effect at the current offset, until the arguments an
    /// `@InlineOnly` body reads in place have been written. `since` is the first entry the
    /// withdrawal may take: the call's own, or [`Self::value_boundary_here`] when the call is the
    /// value of a statement kotlinc marks afterwards. Returns the withdrawn line for
    /// [`Self::settle_deferred_line`], or `None` when no such entry sits here.
    ///
    /// kotlinc's `IrInlineCodegen` generates those arguments before the call marks its line, and
    /// `InplaceArgumentsMethodTransformer` then moves their code, line labels included, to where
    /// the body loads them. A retained or kept entry is an enclosing expression's own and stays.
    pub(crate) fn defer_line_mark(&mut self, since: usize) -> Option<DeferredLine> {
        let pc = u16::try_from(self.bytes.len()).ok()?;
        let retained = self.retained_line_mark == Some(self.line_marks.len().wrapping_sub(1));
        let &(marked, line) = self.line_marks.last()?;
        if retained || self.dead || marked != pc || self.line_marks.len() <= since {
            return None;
        }
        let forgotten = self.pending_lines.forgotten || self.pending_lines.restated_pc == Some(pc);
        while self.line_marks.len() > since && self.line_marks.last().is_some_and(|m| m.0 == pc) {
            self.line_marks.pop();
        }
        self.pending_lines.forgotten = forgotten;
        Some(DeferredLine {
            index: self.line_marks.len(),
            pc,
            line,
            forgotten,
        })
    }

    /// Restore a call-site line [`Self::defer_line_mark`] withdrew, as kotlinc marks it after the
    /// in-place arguments: deduplicated against the line those arguments left in effect. An
    /// unsigned `x + 1u` therefore has its entry on the literal, which marked the call's line
    /// first, while a call whose in-place arguments mark no line keeps it where the body begins.
    pub(crate) fn settle_deferred_line(&mut self, deferred: DeferredLine) {
        let index = deferred.index.min(self.line_marks.len());
        let restate = match self.line_marks[index..].last() {
            Some(&(_, argument_line)) => argument_line != deferred.line,
            None => {
                let consumed =
                    deferred.forgotten && std::mem::take(&mut self.pending_lines.forgotten);
                consumed || self.current_line() != Some(deferred.line)
            }
        };
        if !restate {
            return;
        }
        self.line_marks.insert(index, (deferred.pc, deferred.line));
        if let Some(retained) = self.retained_line_mark.as_mut() {
            if *retained >= index {
                *retained += 1;
            }
        }
    }

    /// How many line entries are recorded so far, for [`Self::withdraw_lines_since`].
    pub(crate) fn line_entry_count(&self) -> usize {
        self.line_marks.len()
    }

    /// Withdraw the line entries recorded after the first `count`: the code emitted since then has
    /// no source position of its own, as kotlinc puts a lambda's captured values on the stack
    /// without one. A retained entry is kept, since it records an inline call site rather than a
    /// start.
    pub(crate) fn withdraw_lines_since(&mut self, count: usize) {
        let retained = self.retained_line_mark.and_then(|index| {
            (index >= count
                && index + 1 == self.line_marks.len()
                && self
                    .line_marks
                    .get(index)
                    .is_some_and(|(pc, _)| usize::from(*pc) == self.bytes.len()))
            .then(|| self.line_marks[index])
        });
        self.line_marks.truncate(count);
        self.retained_line_mark = self.retained_line_mark.filter(|&index| index < count);
        if let Some(retained) = retained {
            self.line_marks.push(retained);
            self.retained_line_mark = Some(self.line_marks.len() - 1);
        }
    }

    /// Withdraw `line` when it was marked at the current offset and no instruction occupies it yet.
    ///
    /// Kotlin's unsigned binary members begin their source position between a plain receiver and a
    /// literal argument, so that eager boundary does not own the receiver load. A mark at an earlier
    /// offset stays: a call such as `toUInt()` has already written its own line there, and a
    /// zero-initializer on the same line is not the literal's to take. A kept mark
    /// ([`Self::mark_line_kept`]) stays too.
    pub(crate) fn withdraw_operand_line(&mut self, line: u32) {
        let line = line.min(u16::MAX as u32) as u16;
        let pc = self.bytes.len();
        // A kept mark is an enclosing expression's own, not this boundary's.
        let kept = self.retained_line_distinct
            && self.retained_line_mark == Some(self.line_marks.len().wrapping_sub(1));
        if !kept
            && self
                .line_marks
                .last()
                .is_some_and(|&(marked_pc, marked)| marked == line && marked_pc as usize == pc)
        {
            let removed = self.line_marks.len() - 1;
            self.line_marks.pop();
            if self.retained_line_mark == Some(removed) {
                self.retained_line_mark = None;
            }
        }
    }

    /// The line in effect was marked on a trailing `nop`.
    ///
    /// A following store on that same line keeps this mark: cleanup removes the `nop` and the
    /// entry lands on the store. Marking the store again would leave the `nop` as the only
    /// instruction of its line, which the cleanup then keeps.
    pub(crate) fn line_marked_on_trailing_nop(&self) -> bool {
        let Some(&(pc, _)) = self.line_marks.last() else {
            return false;
        };
        self.bytes.len() == usize::from(pc) + 1 && self.bytes.last() == Some(&0)
    }

    /// Forget which line is in effect, so the next mark is written even for the same line: kotlinc
    /// resets its last line number after an inlined call (`markLineNumberAfterInlineIfNeeded`),
    /// whose code ran under the callee's lines rather than the caller's.
    pub(crate) fn forget_line(&mut self) {
        self.pending_lines.forgotten = true;
    }

    /// Record an inlined body's line number at the current offset, as ASM copies a
    /// `LineNumberNode`: the entry is written even when the line is already in effect, and a
    /// second entry at one offset follows the first rather than replacing it.
    pub(crate) fn inlined_line(&mut self, line: u16) {
        if self.dead || self.bytes.len() > u16::MAX as usize {
            return;
        }
        let pc = self.bytes.len() as u16;
        self.line_marks.push((pc, line));
        self.inlined_line_marks.push((pc, line));
        self.pending_lines.restated_pc = Some(pc);
    }

    /// Line marks whose provenance is an inlined body or its caller-line restoration.
    pub(crate) fn inlined_line_marks(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.inlined_line_marks
            .iter()
            .copied()
            .filter(|mark| self.line_marks.contains(mark))
    }

    /// Whether the line in effect was forgotten after an inlined body ([`Self::forget_line`]), so
    /// the next mark of any line, the same one included, is written.
    pub(crate) fn line_forgotten(&self) -> bool {
        self.pending_lines.forgotten
    }

    /// The line in effect at the current offset, if any mark has been recorded.
    pub fn current_line(&self) -> Option<u16> {
        self.line_marks.last().map(|&(_, line)| line)
    }

    /// Whether the current line starts on the one-byte `nop` immediately behind the write cursor.
    ///
    /// Inline-frame closure uses that `nop` as a provisional debug anchor. When the caller emits
    /// a real instruction on the same line immediately afterwards, the optimizer may remove the
    /// anchor as redundant; inserting another line boundary ahead of the real instruction would
    /// instead make the `nop` the sole instruction of its debug range and force it to survive.
    pub(crate) fn line_is_anchored_by_trailing_nop(&self, line: u32) -> bool {
        let line = line.min(u16::MAX as u32) as u16;
        self.line_marks.last().is_some_and(|&(pc, marked)| {
            marked == line
                && usize::from(pc).checked_add(1) == Some(self.bytes.len())
                && self.bytes.get(usize::from(pc)) == Some(&0x00)
        })
    }

    /// Record `line` at the current offset as an entry the NEXT mark at that offset must append
    /// after rather than replace.
    ///
    /// kotlinc keeps two entries at one offset where an inline call's site and the first
    /// instruction of what it expands to begin together — `enumValueOf<E>(\n    s\n)` records the
    /// call's line and its argument's line at the same pc. Ordinary marking cannot express that:
    /// the second would overwrite the first and one source position would be lost.
    ///
    /// Retention names the entry this call actually wrote, and nothing when it wrote none. Naming
    /// the OFFSET instead left the flag armed over an offset holding no entry whenever the mark
    /// deduped against an earlier pc: the next mark there fell through to an ordinary push, the one
    /// after it then found the stale flag and appended too, and two entries survived at one offset
    /// where last-wins should have left one.
    pub fn mark_line_retained(&mut self, line: u32) {
        self.retained_line_mark = self.record_line(line);
        self.retained_line_distinct = false;
    }

    /// Whether a kept mark ([`Self::mark_line_kept`]) of `line` stands at the current offset.
    pub(crate) fn line_kept_here(&self, line: u32) -> bool {
        let line = line.min(u16::MAX as u32) as u16;
        let index = self.line_marks.len().wrapping_sub(1);
        self.retained_line_distinct
            && self.retained_line_mark == Some(index)
            && self
                .line_marks
                .get(index)
                .is_some_and(|&(pc, marked)| marked == line && usize::from(pc) == self.bytes.len())
    }

    /// Record `line` as kotlinc's `LineNumberMapper` does wherever a mark precedes code that marks
    /// lines of its own: a later mark of another line at this same offset follows the entry
    /// rather than replacing it, while a mark of the same line is the entry already in effect.
    pub(crate) fn mark_line_kept(&mut self, line: u32) {
        self.retained_line_mark = self.record_line(line);
        self.retained_line_distinct = true;
    }

    /// The recorded `LineNumberTable` marks (empty for a body emitted without line info).
    pub fn line_marks(&self) -> &[(u16, u16)] {
        &self.line_marks
    }
}

#[cfg(test)]
mod tests {
    use super::super::{ClassWriter, CodeBuilder, ACC_PUBLIC, ACC_STATIC};

    /// One instruction, so the next mark lands at a different pc.
    fn advance(code: &mut CodeBuilder) {
        code.aconst_null();
    }

    #[test]
    fn a_line_mark_can_identify_its_trailing_nop_anchor() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(4);
        code.nop();
        assert!(code.line_is_anchored_by_trailing_nop(4));
        assert!(!code.line_is_anchored_by_trailing_nop(5));
        code.dup();
        assert!(!code.line_is_anchored_by_trailing_nop(4));
    }

    #[test]
    fn implicit_void_return_provenance_is_not_inferred_from_the_last_opcode() {
        let mut writer = ClassWriter::new("FooKt", "java/lang/Object");

        let mut implicit = CodeBuilder::new(0);
        implicit.implicit_ret_void();
        writer.add_method(ACC_PUBLIC | ACC_STATIC, "implicit", "()V", &implicit);
        assert_eq!(
            writer.method_implicit_void_return_pc("implicit", "()V"),
            Some(0)
        );

        let mut explicit = CodeBuilder::new(0);
        explicit.ret_void();
        writer.add_method(ACC_PUBLIC | ACC_STATIC, "explicit", "()V", &explicit);
        assert_eq!(
            writer.method_implicit_void_return_pc("explicit", "()V"),
            None
        );

        let mut diverging = CodeBuilder::new(0);
        diverging.aconst_null();
        diverging.athrow();
        writer.add_method(ACC_PUBLIC | ACC_STATIC, "diverging", "()V", &diverging);
        assert_eq!(
            writer.method_implicit_void_return_pc("diverging", "()V"),
            None
        );
    }

    /// The shape the retained operation exists for: a call site and the first instruction of what
    /// it expands to begin together, and BOTH survive.
    #[test]
    fn a_retained_mark_and_one_more_at_its_offset_both_survive() {
        let mut code = CodeBuilder::new(0);
        code.mark_line_retained(3);
        code.mark_line(4);
        assert_eq!(code.line_marks(), [(0, 3), (0, 4)]);
    }

    /// The defect this rule was rewritten for. The retained request is deduped against the line
    /// already in effect at an EARLIER pc, so it writes no entry at the current offset — and must
    /// therefore retain nothing. Naming the offset instead left the flag armed over an offset
    /// holding no entry: the first mark there fell through to an ordinary push, the second found
    /// the stale flag and appended, and two entries survived where last-wins leaves one.
    #[test]
    fn a_retained_request_deduped_against_an_earlier_pc_retains_nothing() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(7);
        advance(&mut code);
        code.mark_line_retained(7); // the line already in effect: no entry here
        code.mark_line(8);
        code.mark_line(9);
        assert_eq!(
            code.line_marks(),
            [(0, 7), (1, 9)],
            "one entry at the second offset: the later mark replaces the earlier one, because \
             nothing was retained there. Naming the offset gave `[(0, 7), (1, 8), (1, 9)]`"
        );
    }

    /// A line marked where positionless code begins is withdrawn, so the line in effect is the
    /// earlier one and the next mark of the withdrawn line is written where it truly begins.
    #[test]
    fn a_withdrawn_mark_leaves_the_earlier_line_in_effect() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(11);
        advance(&mut code);
        code.mark_line(12);
        code.withdraw_line();
        advance(&mut code);
        code.mark_line(12);
        assert_eq!(code.line_marks(), [(0, 11), (2, 12)]);
    }

    /// Retention names an entry, so it expires on its own once the pc moves past it — an ordinary
    /// mark at the new offset is an ordinary mark.
    #[test]
    fn retention_does_not_survive_the_pc_advancing() {
        let mut code = CodeBuilder::new(0);
        code.mark_line_retained(3);
        advance(&mut code);
        code.mark_line(4);
        code.mark_line(5);
        assert_eq!(
            code.line_marks(),
            [(0, 3), (1, 5)],
            "the marks at the new offset are last-wins, not appended"
        );
    }

    /// A retained index remains recorded after its pc has passed, but no longer protects that
    /// capture's line when all of the capture's marks are withdrawn.
    #[test]
    fn withdrawal_does_not_preserve_stale_retention() {
        let mut code = CodeBuilder::new(0);
        let before = code.line_entry_count();
        code.mark_line_kept(3);
        advance(&mut code);
        code.mark_line(4);
        code.withdraw_lines_since(before);
        assert_eq!(code.line_marks(), []);
    }

    /// A live retained call-site entry survives withdrawal, but an earlier ordinary entry from
    /// the same capture does not. Its shifted index still protects it from the next mark.
    #[test]
    fn withdrawal_keeps_only_a_live_retained_entry() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(2);
        advance(&mut code);
        let before = code.line_entry_count();
        code.mark_line(3);
        advance(&mut code);
        code.mark_line_kept(4);
        code.withdraw_lines_since(before);
        assert_eq!(code.line_marks(), [(0, 2), (2, 4)]);

        code.mark_line(5);
        assert_eq!(code.line_marks(), [(0, 2), (2, 4), (2, 5)]);
    }

    /// Dead code marks nothing, and so arms nothing: the statement it would mark is dropped.
    #[test]
    fn a_retained_mark_in_dead_code_arms_nothing() {
        let mut code = CodeBuilder::new(0);
        code.aconst_null();
        code.areturn();
        code.mark_line_retained(3);
        code.mark_line(4);
        assert_eq!(code.line_marks(), [], "no entry, and no retention to spend");
    }

    /// The ordinary rule the retained one is an exception to, stated here so the exception cannot
    /// quietly become the rule: at one offset, the last mark wins.
    #[test]
    fn ordinary_marks_at_one_offset_are_last_wins() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(3);
        code.mark_line(4);
        code.mark_line(5);
        assert_eq!(code.line_marks(), [(0, 5)]);
    }

    /// A mark replaced at its own offset by the line already in effect leaves no entry, and a
    /// forgotten line's restatement survives the same replacement.
    #[test]
    fn a_replacement_by_the_line_in_effect_leaves_no_entry() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(6);
        advance(&mut code);
        code.mark_line(7);
        code.mark_line(6);
        assert_eq!(code.line_marks(), [(0, 6)]);
        code.forget_line();
        code.mark_line(7);
        code.mark_line(6);
        assert_eq!(code.line_marks(), [(0, 6), (1, 6)]);
    }

    /// After an inlined call the line in effect is forgotten, so the next mark of the SAME line is
    /// written again: kotlinc's store of a loop's `last` after the inline `toInt()` of its bound.
    #[test]
    fn a_forgotten_line_is_written_again_by_the_next_mark() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(6);
        advance(&mut code);
        code.mark_line(6);
        assert_eq!(
            code.line_marks(),
            [(0, 6)],
            "an ordinary re-mark deduplicates"
        );
        code.forget_line();
        code.mark_line(6);
        advance(&mut code);
        code.mark_line(6);
        assert_eq!(
            code.line_marks(),
            [(0, 6), (1, 6)],
            "only the first mark after the forget is written"
        );
    }

    /// Inside a condition the line after an inlined call is written at once, for the jump that
    /// follows, even though it is the line already in effect.
    #[test]
    fn an_inlined_line_is_written_even_when_already_in_effect() {
        let mut code = CodeBuilder::new(0);
        code.mark_line(6);
        advance(&mut code);
        code.inlined_line(6);
        code.inlined_line(6);
        assert_eq!(code.line_marks(), [(0, 6), (1, 6), (1, 6)]);
    }

    /// The same line retained and marked again both stay. An unsigned assignment
    /// `r = r or (cur and mask)` records that line twice at the receiver load.
    #[test]
    fn a_retained_mark_keeps_a_second_entry_of_the_same_line() {
        let mut code = CodeBuilder::new(0);
        code.mark_line_retained(8);
        code.mark_line(8);
        assert_eq!(code.line_marks(), [(0, 8), (0, 8)]);
    }

    /// A retained entry is spent by the mark that appends after it: a third at the same offset
    /// replaces the second rather than appending again.
    #[test]
    fn a_spent_retention_does_not_append_twice() {
        let mut code = CodeBuilder::new(0);
        code.mark_line_retained(3);
        code.mark_line(4);
        code.mark_line(5);
        assert_eq!(code.line_marks(), [(0, 3), (0, 5)]);
    }
}
