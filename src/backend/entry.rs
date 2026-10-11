//! Which function a whole program starts in, and how a `box()` program reports its answer.
//!
//! Every backend that produces a runnable program — a native executable, a WebAssembly module —
//! starts it the same two ways, and a conformance harness reads every target's answer the same
//! way. So the contract lives at the backend boundary rather than in any one target.

/// What a `box()` program prints before its answer. A harness requires exactly one occurrence and
/// reads every byte after it, so an answer that spans lines is kept whole and an answer/program
/// output containing the marker fails rather than spoofing a verdict. The NULs keep ordinary
/// output from spelling it accidentally.
pub const BOX_RESULT_FRAME: &str = "\u{0}krusty box result\u{0}";

/// Which top-level function a program starts in. The frontend selects each kind once per source
/// unit; [`Entry::selected`] reads that choice from a checked file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// Kotlin's `fun main()`.
    Main,
    /// A `codegen/box` conformance case: `fun box(): String`, whose result the entry prints after
    /// [`BOX_RESULT_FRAME`] — so the case's verdict (`OK`, or what went wrong) is the program's
    /// output, with no `main` written into the corpus.
    Box,
}

impl Entry {
    /// The function `ir` starts this kind of program in, as the frontend selected it, and its
    /// recorded parameter form. Backends consume that form according to their representation
    /// support; they never reselect the declaration from its spelling.
    pub(crate) fn selected(
        self,
        ir: &crate::ir::IrFile,
    ) -> Option<(crate::ir::FunId, crate::ir::MainEntryParameters)> {
        match self {
            Entry::Main => ir
                .entry_point
                .map(|entry| (entry.function, entry.parameters)),
            Entry::Box => ir
                .box_entry
                .map(|function| (function, crate::ir::MainEntryParameters::None)),
        }
    }

    /// How a diagnostic names this entry.
    pub fn declaration(self) -> &'static str {
        match self {
            Entry::Main => "`fun main()`",
            Entry::Box => "`fun box(): String`",
        }
    }
}
