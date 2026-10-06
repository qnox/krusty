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
        let (needs_lnt, needs_lvt, suppress_entry_line) = match self
            .methods
            .iter()
            .find(|method| method.name == n && method.desc == d)
        {
            Some(method) => (
                method.lnt.is_empty(),
                method.lvt.is_empty(),
                method.suppress_entry_line,
            ),
            None => return,
        };
        if !needs_lnt && !suppress_entry_line {
            if let Some((0, line)) = lnt {
                if let Some(method) = self
                    .methods
                    .iter_mut()
                    .find(|method| method.name == n && method.desc == d)
                {
                    let line = line.min(u16::MAX as u32) as u16;
                    match method.lnt.first() {
                        Some(&(0, _)) => {}
                        Some(&(_, first)) if first == line => {}
                        _ => method.lnt.insert(0, (0, line)),
                    }
                }
            }
        }
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
            // Already exactly this entry.
            Some((lpc, ll)) if *lpc == pc && *ll == line => Some(self.line_marks.len() - 1),
            // A retained entry sits here: this mark goes AFTER it, so both survive. The retention
            // is spent — a third mark at the same offset replaces this one as usual.
            Some((lpc, _)) if *lpc == pc && retained => {
                self.retained_line_mark = None;
                self.line_marks.push((pc, line));
                Some(self.line_marks.len() - 1)
            }
            Some((lpc, _)) if *lpc == pc => {
                let last = self.line_marks.last_mut()?;
                last.1 = line;
                Some(self.line_marks.len() - 1)
            }
            // The line already in effect, carried from an EARLIER pc: no entry is written here, so
            // this offset holds none of this mark's to retain.
            Some((_, ll)) if *ll == line && !forgotten => None,
            _ => {
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

    /// Withdraw the current `line` mark before an operation emits its receiver. Kotlin's unsigned
    /// binary members begin their source position between receiver and argument, so their eager
    /// root-expression boundary does not own the receiver load.
    pub(crate) fn withdraw_operand_line(&mut self, line: u32) {
        let line = line.min(u16::MAX as u32) as u16;
        if self
            .line_marks
            .last()
            .is_some_and(|&(_, marked)| marked == line)
        {
            let removed = self.line_marks.len() - 1;
            self.suppress_entry_line |= self.line_marks[removed].0 == 0;
            self.line_marks.pop();
            if self.retained_line_mark == Some(removed) {
                self.retained_line_mark = None;
            }
        }
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
        if self.line_marks.last() != Some(&(pc, line)) {
            self.line_marks.push((pc, line));
            self.inlined_line_marks.push((pc, line));
        }
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
        assert_eq!(code.line_marks(), [(0, 6), (1, 6)]);
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
