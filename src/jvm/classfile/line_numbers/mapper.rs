//! kotlinc's `LineNumberMapper`: whether a line mark writes a `LineNumberTable` entry.
//!
//! A mark of the line marked last writes nothing, an inlined call makes the mapper forget that
//! line (or, inside a condition, registers it again at once), and every other mark appends an
//! entry, a second one at the same offset included. Which IR node marks a line, and where, is the
//! JVM backend's node-kind rule; this is only the mapper that rule writes through.

/// kotlinc's `LineNumberMapper` for one method body: the line it marked last and whether marks
/// are suppressed.
#[derive(Clone, Default)]
pub struct LineNumberMapper {
    /// `lastLineNumber`; `None` is kotlinc's `-1`, before the first mark and after an inlined
    /// call.
    last_line: Option<u16>,
    /// Open `noLineNumberScope`s.
    no_line_scopes: u32,
}

impl LineNumberMapper {
    /// `markLineNumber`: whether `line` is due an entry, which is when it is not the line marked
    /// last and no scope suppresses marks. A due line becomes the line marked last.
    pub(crate) fn mark(&mut self, line: u16) -> bool {
        if self.no_line_scopes > 0 || self.last_line == Some(line) {
            return false;
        }
        self.last_line = Some(line);
        true
    }

    /// `markLineNumberAfterInlineIfNeeded`: the line to register again at once, inside a condition
    /// or a no-line scope; elsewhere the mapper forgets its line, so the next mark of any line,
    /// the same one included, writes an entry.
    pub(crate) fn after_inline(&mut self, register: bool) -> Option<u16> {
        if register || self.no_line_scopes > 0 {
            self.last_line
        } else {
            self.last_line = None;
            None
        }
    }

    /// `lastLineNumber`.
    pub(crate) fn last_line(&self) -> Option<u16> {
        self.last_line
    }

    /// Open a `noLineNumberScope`: marks write nothing until it is closed.
    pub fn open_no_line_scope(&mut self) {
        self.no_line_scopes += 1;
    }

    /// Close the innermost `noLineNumberScope`.
    pub fn close_no_line_scope(&mut self) {
        self.no_line_scopes = self.no_line_scopes.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::LineNumberMapper;

    #[test]
    fn a_mark_of_the_line_marked_last_writes_nothing() {
        let mut mapper = LineNumberMapper::default();
        assert!(mapper.mark(3));
        assert!(!mapper.mark(3));
        assert!(mapper.mark(4));
        assert!(mapper.mark(3));
    }

    #[test]
    fn an_inlined_call_forgets_the_line_outside_a_condition() {
        let mut mapper = LineNumberMapper::default();
        assert!(mapper.mark(7));
        assert_eq!(mapper.after_inline(false), None);
        assert_eq!(mapper.last_line(), None);
        assert!(mapper.mark(7));
    }

    #[test]
    fn an_inlined_call_inside_a_condition_registers_the_line_again() {
        let mut mapper = LineNumberMapper::default();
        assert!(mapper.mark(7));
        assert_eq!(mapper.after_inline(true), Some(7));
        assert!(!mapper.mark(7));
    }

    #[test]
    fn a_no_line_scope_writes_nothing_and_keeps_the_line() {
        let mut mapper = LineNumberMapper::default();
        assert!(mapper.mark(2));
        mapper.open_no_line_scope();
        assert!(!mapper.mark(5));
        mapper.close_no_line_scope();
        assert_eq!(mapper.last_line(), Some(2));
        assert!(mapper.mark(5));
    }
}
