//! `LineNumberTable` marking.
//!
//! One method's source positions are recorded as (pc, line) pairs while its bytecode is built. The
//! rules about which mark wins at a given offset live here, beside the table they produce, rather
//! than at the emission sites that call them.

use super::CodeBuilder;

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
    fn record_line(&mut self, line: u32) -> Option<usize> {
        if self.dead {
            return None; // the statement it would mark is dropped dead code (see `dead`)
        }
        if self.bytes.len() > u16::MAX as usize {
            return None; // past the classfile pc range — an entry would silently wrap
        }
        let line = line.min(u16::MAX as u32) as u16;
        let pc = self.bytes.len() as u16;
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
            Some((_, ll)) if *ll == line => None,
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

    /// The line in effect at the current offset, if any mark has been recorded.
    pub fn current_line(&self) -> Option<u16> {
        self.line_marks.last().map(|&(_, line)| line)
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
    use super::super::CodeBuilder;

    /// One instruction, so the next mark lands at a different pc.
    fn advance(code: &mut CodeBuilder) {
        code.aconst_null();
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
