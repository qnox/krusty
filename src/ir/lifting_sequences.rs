//! The sequences kotlinc numbers the local callables it lifts in.

/// One sequence of lifted local callables: the source file that declares it, the lexical owner,
/// and the outermost declaration name, as a [`crate::fir::FirLiftingSite`] spells them.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct IrLiftingSequence {
    pub source: crate::fir::SourceFileId,
    pub owner: Box<str>,
    pub container: Box<str>,
}

/// The outermost position of the path a callable was lifted along: the declaration whose body it
/// is written in, through every enclosing local callable. A callable that stops being lifted (a
/// lambda realized as a class of its own) keeps it, so a target can still find that declaration's
/// realization once its own passes have placed it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IrLiftingRoot {
    pub sequence: IrLiftingSequence,
    pub position: u32,
}

/// One position of an [`IrLiftingSequence`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IrLiftingEntry {
    pub kind: crate::lifting_provenance::LiftingCallableKind,
    pub name: Option<Box<str>>,
    pub lifted: bool,
    /// The position of the innermost lambda this entry is nested in, whose own numbering it takes
    /// part in; `None` for one numbered by the sequence itself.
    pub scope: Option<u32>,
    /// The declaration whose body writes this entry. Overloads that share the sequence's source
    /// name are distinct containers; a target whose ABI renames one of them names and numbers its
    /// lifted callables after that realization.
    pub container: Option<super::IrEnclosure>,
}
