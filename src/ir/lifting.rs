//! The sequences kotlinc numbers lifted local callables in, as common IR carries them.

/// One sequence of lifted local callables: the source file that declares it, the lexical owner,
/// and the outermost declaration name, as a [`crate::fir::FirLiftingSite`] spells them.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct IrLiftingSequence {
    pub source: crate::fir::SourceFileId,
    pub owner: Box<str>,
    pub container: Box<str>,
}

/// One position of an [`IrLiftingSequence`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IrLiftingEntry {
    pub name: Option<Box<str>>,
    pub lifted: bool,
}
