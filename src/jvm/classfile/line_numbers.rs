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
        if self.dead {
            return; // the statement it would mark is dropped dead code (see `dead`)
        }
        if self.bytes.len() > u16::MAX as usize {
            return; // past the classfile pc range — an entry would silently wrap
        }
        let line = line.min(u16::MAX as u32) as u16;
        let pc = self.bytes.len() as u16;
        let retained = self.retained_line_offset == Some(pc);
        match self.line_marks.last() {
            // Already exactly this entry.
            Some((lpc, ll)) if *lpc == pc && *ll == line => {}
            // A retained entry sits here: this mark goes AFTER it, so both survive. The retention
            // is spent — a third mark at the same offset replaces this one as usual.
            Some((lpc, _)) if *lpc == pc && retained => {
                self.retained_line_offset = None;
                self.line_marks.push((pc, line));
            }
            Some((lpc, _)) if *lpc == pc => {
                if let Some((_, last)) = self.line_marks.last_mut() {
                    *last = line;
                }
            }
            Some((_, ll)) if *ll == line => {}
            _ => self.line_marks.push((pc, line)),
        }
    }

    /// Record `line` at the current offset as an entry the NEXT mark at that offset must append
    /// after rather than replace.
    ///
    /// kotlinc keeps two entries at one offset where an inline call's site and the first
    /// instruction of what it expands to begin together — `enumValueOf<E>(\n    s\n)` records the
    /// call's line and its argument's line at the same pc. Ordinary marking cannot express that:
    /// the second would overwrite the first and one source position would be lost.
    pub fn mark_line_retained(&mut self, line: u32) {
        self.mark_line(line);
        if !self.dead && self.bytes.len() <= u16::MAX as usize {
            self.retained_line_offset = Some(self.bytes.len() as u16);
        }
    }

    /// The recorded `LineNumberTable` marks (empty for a body emitted without line info).
    pub fn line_marks(&self) -> &[(u16, u16)] {
        &self.line_marks
    }
}
