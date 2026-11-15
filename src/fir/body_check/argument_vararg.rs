//! The coordinate in which argument mapping names a call's `vararg` parameter.

use super::{BodyCheckFailure, BodyCheckFailureKind, BodyFirChecker};
use crate::diag::Span;

/// Where a call's `vararg` sits among the parameters its source arguments address: after the
/// callable's leading context parameters, which no source argument fills positionally.
///
/// A declaration records its vararg among all of its parameters, contexts included. Argument
/// mapping accepts only this type, so every call target converts that declared position once, by
/// its own context count, and a declared index cannot reach mapping unconverted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ArgumentVararg(usize);

/// A declared vararg position inside the context-parameter prefix: no source argument can reach
/// it, so the selected callable contradicts its own shape.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct VarargAmongContexts;

impl ArgumentVararg {
    /// The vararg a declaration records at `declared` among its parameters, the first `contexts`
    /// of which are context parameters. `Ok(None)` is a callable without a vararg.
    pub(super) fn of_declaration(
        declared: Option<usize>,
        contexts: usize,
    ) -> Result<Option<Self>, VarargAmongContexts> {
        declared
            .map(|index| {
                index
                    .checked_sub(contexts)
                    .map(Self)
                    .ok_or(VarargAmongContexts)
            })
            .transpose()
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

impl BodyFirChecker<'_> {
    /// [`ArgumentVararg::of_declaration`] for the call at `span`; a vararg among the contexts is
    /// an unsupported call shape.
    pub(super) fn declared_argument_vararg(
        &self,
        span: Option<Span>,
        declared: Option<usize>,
        contexts: usize,
    ) -> Result<Option<ArgumentVararg>, BodyCheckFailure> {
        ArgumentVararg::of_declaration(declared, contexts).map_err(|VarargAmongContexts| {
            self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ArgumentVararg, VarargAmongContexts};

    #[test]
    fn a_declared_vararg_converts_past_the_context_parameters() {
        assert_eq!(ArgumentVararg::of_declaration(None, 2), Ok(None));
        assert_eq!(
            ArgumentVararg::of_declaration(Some(3), 2)
                .map(|vararg| vararg.map(ArgumentVararg::index)),
            Ok(Some(1))
        );
        assert_eq!(
            ArgumentVararg::of_declaration(Some(0), 0)
                .map(|vararg| vararg.map(ArgumentVararg::index)),
            Ok(Some(0))
        );
        assert_eq!(
            ArgumentVararg::of_declaration(Some(0), 1),
            Err(VarargAmongContexts)
        );
        assert_eq!(
            ArgumentVararg::of_declaration(Some(1), 2),
            Err(VarargAmongContexts)
        );
    }
}
