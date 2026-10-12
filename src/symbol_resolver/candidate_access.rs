//! Accessibility filtering before argument mapping and overload selection.

use super::{
    level_functions, receiver_allows_member_dispatch, Callables, FnKind, FunctionInfo, SymRecv,
    SymbolResolver, Ty,
};

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

    /// The member function overloads `recv.name` denotes: the accessible member part of
    /// [`SymbolResolver::select_symbol`]'s overload family, without selecting a call among them.
    pub(crate) fn member_overloads(&self, recv: SymRecv, name: &str) -> Vec<FunctionInfo> {
        let members = |functions: &[FunctionInfo]| {
            functions
                .iter()
                .filter(|overload| overload.kind == FnKind::Member)
                .cloned()
                .collect::<Vec<_>>()
        };
        match recv {
            SymRecv::Value(ty) | SymRecv::ImplicitValue(ty) => {
                if !receiver_allows_member_dispatch(ty) {
                    return Vec::new();
                }
                members(
                    self.receiver_callable_inventory(ty, name)
                        .accessible
                        .functions(),
                )
            }
            SymRecv::TopLevel => self
                .symbol_levels_in_scope(name)
                .iter()
                .find(|symbols| level_functions(symbols).next().is_some())
                .map(|symbols| {
                    level_functions(symbols)
                        .filter(|overload| overload.kind == FnKind::Member)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
            SymRecv::Type(_) | SymRecv::TypeName(_) => Vec::new(),
        }
    }

    pub(super) fn retain_accessible_extensions(
        &self,
        candidates: &mut Vec<(u32, Ty, &FunctionInfo)>,
        inaccessible: &mut Vec<FunctionInfo>,
        hides_members: bool,
    ) {
        let hides_members_annotation = crate::types::type_name("kotlin/internal/HidesMembers");
        candidates.retain(|(_, _, candidate)| {
            candidate
                .callable
                .annotations
                .contains(&hides_members_annotation)
                == hides_members
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
        self.visibility_suppressed
            || crate::callable_access::source_callable_accessible(
                candidate.visibility,
                candidate.source_file,
                self.access_file,
                || {
                    self.lib
                        .dependency_access_policy()
                        .internal_callable_accessible(
                            candidate.callable.external_identity,
                            candidate.callable.owner,
                        )
                },
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
