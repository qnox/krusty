//! Accessibility filtering before argument mapping and overload selection.

use super::{FunctionInfo, SymbolResolver};

impl SymbolResolver<'_> {
    /// File/module accessibility for a top-level function or extension declaration.
    pub(crate) fn non_member_callable_accessible(&self, candidate: &FunctionInfo) -> bool {
        crate::callable_access::source_callable_accessible(
            candidate.visibility,
            candidate.source_file,
            self.access_file,
            || self.lib.internal_accessible(candidate.callable.owner),
        )
    }

    /// Candidates that may participate in source-argument mapping for this access context.
    pub(crate) fn accessible_top_level_candidates(&self, name: &str) -> Vec<FunctionInfo> {
        self.top_level_candidates(name)
            .into_iter()
            .filter(|candidate| self.non_member_callable_accessible(candidate))
            .collect()
    }
}
