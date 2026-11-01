//! The coordinate in which argument mapping names a call's `vararg` parameter.

/// Where a call's `vararg` sits among the parameters its source arguments address: after the
/// callable's leading context parameters, which no source argument fills positionally.
///
/// A declaration records its vararg among all of its parameters, contexts included. Argument
/// mapping accepts only this type, so every call target converts that declared position once, by
/// its own context count, and a declared index cannot reach mapping unconverted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ArgumentVararg(usize);

impl ArgumentVararg {
    /// The vararg a declaration records at `declared` among its parameters, the first `contexts`
    /// of which are context parameters.
    pub(super) fn of_declaration(declared: Option<usize>, contexts: usize) -> Option<Self> {
        declared
            .and_then(|index| index.checked_sub(contexts))
            .map(Self)
    }

    /// A vararg the selected call already records among the parameters its source arguments
    /// address: an extension call's value parameters, or a call with no context parameters.
    pub(super) fn of_source_parameters(index: Option<usize>) -> Option<Self> {
        index.map(Self)
    }

    pub(super) const fn index(self) -> usize {
        self.0
    }
}
