//! Accessibility filtering before argument mapping and overload selection.

use super::{Callables, FunctionInfo, SymbolResolver, Ty};

pub(crate) struct ReceiverCallableInventory {
    pub(crate) accessible: Callables,
    pub(crate) inaccessible_extensions: Vec<FunctionInfo>,
}

impl SymbolResolver<'_> {
    /// Collect one receiver/name inventory while keeping inaccessible extensions outside the
    /// candidate family consumed by argument mapping and overload selection.
    pub(crate) fn receiver_callable_inventory(
        &self,
        receiver: Ty,
        name: &str,
    ) -> ReceiverCallableInventory {
        self.collect_receiver_callables(receiver, name)
    }

    pub(crate) fn receiver_callables(&self, receiver: Ty, name: &str) -> Callables {
        self.receiver_callable_inventory(receiver, name).accessible
    }

    pub(super) fn retain_accessible_extensions(
        &self,
        candidates: &mut Vec<(u32, Ty, &FunctionInfo)>,
        inaccessible: &mut Vec<FunctionInfo>,
        hides_members: bool,
    ) {
        let hides_members_annotation = crate::types::type_name("kotlin/internal/HidesMembers");
        candidates.retain(|(_, _, candidate)| {
            candidate.annotations.contains(&hides_members_annotation) == hides_members
        });
        inaccessible.extend(
            candidates
                .iter()
                .filter(|(_, _, candidate)| !self.non_member_callable_accessible(candidate))
                .map(|(_, _, candidate)| (*candidate).clone()),
        );
        candidates.retain(|(_, _, candidate)| self.non_member_callable_accessible(candidate));
    }

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
