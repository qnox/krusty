//! Relocated bytecode and side tables produced by one inline splice.

/// The result of splicing a body, laid out at the `start_offset` passed to `splice_unified`.
/// Stack-map frames are deliberately absent: the class writer derives them from the final body after
/// every splice and rewrite.
pub struct SpliceResult {
    pub bytes: Vec<u8>,
    /// Whether byte offsets in the result depend on its final position in the enclosing method.
    /// Switch padding and absolute side-table/fixup offsets require a second layout at the real
    /// position. Ordinary relative branches do not, and this is deliberately unrelated to frames.
    pub needs_relayout: bool,
    /// Whether the transformed entry-to-end control-flow graph reaches the continuation after this
    /// splice. A method body with no reachable return (for example `TODO`, whose only exit is
    /// `athrow`) leaves the enclosing bytecode path unreachable even though its bytes were appended
    /// in bulk.
    pub falls_through: bool,
    /// The body's exception table, relocated into the caller: `(start, end, handler, catch_type)` as
    /// ABSOLUTE byte offsets in the spliced output, with `catch_type` re-interned into `cw` (0 =
    /// catch-all/`finally`). Empty for a body with no handlers.
    pub handlers: Vec<(usize, usize, usize, u16)>,
    /// Branch operands (absolute positions in the enclosing builder) whose destinations live outside
    /// this splice. The enclosing builder retains them until their owning loop label is bound.
    pub external_branches: Vec<(usize, super::super::classfile::Label)>,
    /// The dependency's own debug locals, relocated into the caller: `(start, length, slot, name,
    /// descriptor)` with ABSOLUTE offsets in the spliced output and the caller's slot numbering.
    ///
    /// A spliced body's locals are real locals of the method that now contains them, and the
    /// reference compiler describes every one of them. Their names come from the dependency, which
    /// is the only place they exist; the `$iv` suffix marking a value as inlined is added here,
    /// because it is a property of being inlined rather than of the declaration.
    pub locals: Vec<(u16, u16, u16, String, String)>,
    /// Line marks for the spliced code: `(absolute offset, line, from the dependency)`, ascending.
    ///
    /// A host instruction's line belongs to the DEPENDENCY's file and means nothing until the
    /// caller's source map gives it an output line, which is what the flag distinguishes. A spliced
    /// lambda's body is the caller's own source and keeps its own lines.
    pub lines: Vec<(u16, u16, bool)>,
}
