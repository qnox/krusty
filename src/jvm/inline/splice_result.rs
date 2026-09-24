//! Relocated bytecode and verifier state produced by one inline splice.

use super::VType;

/// The result of splicing a **branchy** body: the spliced bytes (laid out at the `start_offset` passed
/// to `splice_unified`) plus the relocated `StackMapTable` frames the caller must add (each: ABSOLUTE
/// byte offset, the *body* locals at that point, and the operand stack). The caller prepends its own
/// locals (slots `0..base`). The **join** is where the body's returns land (empty body locals + the
/// return value on the stack), bound by the caller right after the spliced bytes.
pub struct BranchySplice {
    pub bytes: Vec<u8>,
    /// Frames *inside* the body: (ABSOLUTE byte offset, body locals, stack). The caller prepends its own
    /// locals and binds at the offset directly.
    pub frames: Vec<(usize, Vec<VType>, Vec<VType>)>,
    /// The operand stack at the **join** (where the body's returns land = the continuation right after
    /// `bytes`): the return value, or empty for `void`. The caller binds this frame at the live
    /// post-splice position (not a precomputed end offset, which could fall at `code.len()`).
    pub join_stack: Vec<VType>,
    /// Whether the splice actually needs the relocated frames + a join frame bound (so it requires an
    /// empty operand-stack baseline). `false` for a pure BRANCHLESS body — no branches, the single
    /// trailing return dropped to fall through — which the caller can then append at ANY stack height
    /// (mid-expression), exactly like the former `splice_branchless`.
    pub join_required: bool,
    /// Whether the transformed entry-to-end control-flow graph reaches the continuation after this
    /// splice. A method body with no reachable return (for example `TODO`, whose only exit is
    /// `athrow`) leaves the enclosing bytecode path unreachable even though its bytes were appended
    /// in bulk.
    pub falls_through: bool,
    /// Every replaced lambda invocation. A single inline parameter can be invoked repeatedly; each
    /// occurrence has its own byte position and host verifier state while referring back to the one
    /// pre-built lambda body by `lambda_index`.
    pub lambda_sites: Vec<RelocatedLambdaSite>,
    /// Operand-stack height the body needs beyond its own `max_stack`: an expanded `typeOf`
    /// realization replaces a one-word placeholder with a deeper sequence.
    pub stack_growth: u16,
    /// The body's exception table, relocated into the caller: `(start, end, handler, catch_type)` as
    /// ABSOLUTE byte offsets in the spliced output, with `catch_type` re-interned into `cw` (0 =
    /// catch-all/`finally`). The handler frames themselves are already in `frames` (a handler is a
    /// StackMapTable target). Empty for a body with no handlers.
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

pub struct RelocatedLambdaSite {
    pub lambda_index: usize,
    /// Which of that lambda's `bodies` this site received.
    pub body_index: usize,
    pub byte_start: usize,
    pub host_locals: Vec<VType>,
    pub stack_prefix: Option<Vec<VType>>,
}
