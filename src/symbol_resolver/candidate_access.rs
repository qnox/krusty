//! Accessibility filtering before argument mapping and overload selection.

use super::{
    level_functions, level_properties, member_scope_receiver, members_in_hierarchy,
    ranked_extension_candidates, receiver_allows_member_dispatch, supports_member_lookup,
    Callables, CandidateSelection, FnKind, FunctionInfo, FunctionSet, InheritedNestedClassifier,
    PropKind, PropertySet, SymRecv, SymbolResolver, Ty,
};

pub(crate) struct ReceiverCallableInventory {
    pub(crate) accessible: Callables,
    pub(crate) inaccessible_extensions: Vec<FunctionInfo>,
}

impl SymbolResolver<'_> {
    /// Collect the declarations denoted by `receiver.name` exactly once. Member declarations and
    /// imported extension declarations use the same [`Callables`] shape; overload selection is a
    /// separate operation over this family.
    fn collect_receiver_callables(&self, receiver: Ty, name: &str) -> ReceiverCallableInventory {
        // `Nothing` is the bottom value type, but its member scope is still Kotlin's ordinary
        // `Any` scope. This matters on unreachable/safe selector branches (`x == null; x?.equals(1)`):
        // the receiver remains `Nothing` for flow typing and extension applicability, while member
        // inventory comes from the semantic top classifier. No runtime receiver conversion is
        // implied; checked FIR retains the original bottom-typed expression.
        let member_scope_receiver = member_scope_receiver(receiver);
        let members = if !member_scope_receiver.is_nullable()
            && supports_member_lookup(member_scope_receiver)
        {
            members_in_hierarchy(&self.src, member_scope_receiver, name)
        } else {
            Callables::default()
        };
        let mut functions = members
            .functions()
            .iter()
            .filter(|function| function.kind == FnKind::Member)
            .cloned()
            .collect::<Vec<_>>();
        let mut properties = members
            .properties()
            .iter()
            .filter(|property| property.kind == PropKind::Member)
            .cloned()
            .collect::<Vec<_>>();
        let mut inaccessible_extensions = Vec::new();
        let levels = self
            .fn_scope
            .map(|scope| {
                super::scope_level_callables::declaration_levels_in_function_scope(
                    &self.src, name, scope,
                )
            })
            .unwrap_or_default();
        if self.fn_scope.is_some() {
            // `@kotlin.internal.HidesMembers` declarations are NOT part of the tower: they resolve
            // above all of it. The nearest-level rule below stops at the first level holding an
            // applicable extension, which would hide the default-imported `kotlin.collections`
            // level behind a same-file / imported / local `forEach` — so lift every annotated
            // declaration applicable to this receiver out of EVERY level first. Selection, not
            // lookup, then decides whether the promotion actually takes the call (see the priority
            // tiers in `select_overload_tracking_with_functions`); an annotated declaration that
            // does not fit falls through to the ordinary candidates collected below.
            for level in &levels {
                let mut ranked = ranked_extension_candidates(
                    &self.src,
                    receiver,
                    self.type_variables,
                    level_functions(&level.symbols),
                );
                self.retain_accessible_extensions(&mut ranked, &mut inaccessible_extensions, true);
                functions.extend(ranked.into_iter().map(|(rank, _, function)| {
                    let mut function = function.clone();
                    function.receiver_rank = rank;
                    function.scope_rung = level.kind.callable_scope_rung();
                    function
                }));
            }
            let mut extension_property_level_found = false;
            for level in &levels {
                let scope_rank = level.rank;
                crate::trace_compiler!(
                    "resolve",
                    "receiver scope level name={name} receiver={receiver:?} functions={:?}",
                    level_functions(&level.symbols)
                        .map(|function| (function.kind, function.semantic_receiver()))
                        .collect::<Vec<_>>()
                );
                // Annotated declarations were already lifted above the tower; collecting them a second
                // time here would put two copies of one declaration in the same priority bucket and
                // read as an ambiguity. A level holding ONLY annotated declarations is therefore
                // empty for tower purposes and the walk continues past it.
                let mut extensions = ranked_extension_candidates(
                    &self.src,
                    receiver,
                    self.type_variables,
                    level_functions(&level.symbols),
                );
                self.retain_accessible_extensions(
                    &mut extensions,
                    &mut inaccessible_extensions,
                    false,
                );
                crate::trace_compiler!(
                    "resolve",
                    "receiver scope applicable name={name} receiver={receiver:?} extensions={:?}",
                    extensions
                        .iter()
                        .map(|(rank, binding, function)| (
                            rank,
                            binding,
                            function.semantic_receiver()
                        ))
                        .collect::<Vec<_>>()
                );
                let extension_properties = if extension_property_level_found {
                    Vec::new()
                } else {
                    level_properties(&level.symbols)
                        .filter(|property| property.kind == PropKind::Extension)
                        .cloned()
                        .collect::<Vec<_>>()
                };
                if extensions.is_empty() && extension_properties.is_empty() {
                    continue;
                }
                functions.extend(extensions.into_iter().map(|(rank, _, function)| {
                    let mut function = function.clone();
                    function.receiver_rank = rank;
                    function.scope_rank = scope_rank;
                    function.scope_rung = level.kind.callable_scope_rung();
                    function
                }));
                extension_property_level_found |= !extension_properties.is_empty();
                properties.extend(extension_properties);
            }
        }
        if let CandidateSelection::Selected(selected) =
            self.scoped_receiver_constructor_classifier(name, &levels)
        {
            if selected.alias.is_some()
                && matches!(
                    self.bound_inner_classifier(receiver, name),
                    InheritedNestedClassifier::NotFound
                )
            {
                functions
                    .extend(self.bound_inner_alias_constructor_candidates(receiver, &selected));
            }
        }
        ReceiverCallableInventory {
            accessible: Callables::from_parts(
                FunctionSet {
                    overloads: functions,
                },
                PropertySet {
                    overloads: properties,
                },
            ),
            inaccessible_extensions,
        }
    }

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

    /// The extensions named `name` at the innermost scope level declaring any, whatever their
    /// receiver, among the levels the file brings into scope itself; default imports never count.
    /// When nothing of that name applies to an explicit receiver, kotlinc reports these as the
    /// candidates a receiver type mismatch rejected rather than an unresolved reference.
    pub(crate) fn receiver_mismatched_extensions(&self, name: &str) -> Vec<FunctionInfo> {
        let Some(scope) = self.fn_scope else {
            return Vec::new();
        };
        super::scope_level_callables::tagged_symbol_levels_in_function_scope(&self.src, name, scope)
            .iter()
            .filter(|level| level.is_written_scope())
            .map(|level| {
                level_functions(&level.symbols)
                    .filter(|function| function.is_extension())
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .find(|extensions| !extensions.is_empty())
            .unwrap_or_default()
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
