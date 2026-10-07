//! Receiver views committed after selection from a semantic intersection.

use super::*;

impl Checker<'_> {
    /// Project a flow intersection onto the constituent that owns the member being used. Candidate
    /// collection and overload selection remain the resolver's job; this operation only chooses the
    /// receiver view exposed by the proven intersection. When two bounds contribute the same
    /// parameter shape, Kotlin's synthetic intersection override has the covariant result, so the
    /// bound with the uniquely most-specific result is the correct projection regardless of test
    /// order (`A.foo(): Any?`, `B.foo(): String`).
    pub(super) fn flow_intersection_member_receiver(
        &self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
        name: &str,
    ) -> Option<Ty> {
        let path = self.expr_access_path(receiver)?;
        let bounds = self.lookup_intersection_narrowing(scope, &path);
        self.intersection_member_receiver(&bounds, name)
    }

    /// Project a flow intersection onto the constituent that supplies the selected property facet.
    /// A write requires a setter on that constituent; a read accepts the ordinary getter facet.
    pub(super) fn flow_intersection_property_receiver(
        &self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
        name: &str,
        write: bool,
    ) -> Option<Ty> {
        let path = self.expr_access_path(receiver)?;
        let bounds = self.lookup_intersection_narrowing(scope, &path);
        self.intersection_property_receiver(&bounds, name, write)
    }

    /// Select a property fake override inherited through several direct semantic supertypes. The
    /// nominal receiver remains the runtime value; checked FIR carries the chosen supertype view.
    pub(super) fn nominal_intersection_property_receiver(
        &self,
        receiver: Ty,
        name: &str,
        write: bool,
    ) -> Option<Ty> {
        let source = self.fed_source();
        // A nominal declaration is already Kotlin's selected override. Projecting it onto one of
        // its direct supertypes discards a covariant result (`B.result: String` would become
        // `A.result: Any`). Only a classifier with no usable declaration of its own needs the
        // synthetic intersection/fake-override selection below.
        let has_declared_facet =
            crate::symbol_resolver::declared_member_callables(&source, receiver, name)
                .properties()
                .iter()
                .any(|property| {
                    property.kind == crate::libraries::PropKind::Member
                        && property.receiver_rank == 0
                        && (!write || property.setter.is_some())
                });
        if has_declared_facet {
            return None;
        }
        let bounds = crate::symbol_resolver::direct_supertypes(&source, receiver);
        // A fake override may combine a getter inherited from one direct supertype with the sole
        // compatible setter inherited from another (`A.val x` plus `B.var x`). For a write, the
        // setter-owning constituent is the semantic receiver view even though it is the only
        // writable contributor.
        if write {
            return self.intersection_property_receiver(&bounds, name, true);
        }
        // A single property-bearing parent is ordinary inheritance, not an intersection. Keep the
        // nominal receiver so nearer accessor overrides can refine a provider-derived property.
        let resolver = self.resolver();
        let contributors = bounds
            .iter()
            .filter(|&&bound| resolver.select_member_property(bound, name).is_some())
            .count();
        if contributors < 2 {
            return None;
        }
        self.intersection_property_receiver(&bounds, name, write)
    }

    /// Project a type-parameter receiver onto the constituent of its declared intersection that
    /// owns the selected member. Checked FIR sees the concrete view whose dispatch target was
    /// selected, including the cast needed for a member declared only on a later bound.
    pub(super) fn type_parameter_member_receiver(
        &self,
        scope: &CheckerScope<'_>,
        receiver: Ty,
        name: &str,
    ) -> Option<Ty> {
        let primary = receiver.non_null().ty_param_bound()?;
        let mut bounds = vec![primary];
        bounds.extend(self.semantic_tparam_extra_bounds(scope, receiver));
        self.intersection_member_receiver(&bounds, name)
    }

    /// The constituent that exposes the exact declaration selected from an inferred intersection.
    /// Candidate collection must see the complete intersection first: projecting by spelling can
    /// discard overloads or choose the wrong fake override. Once overload selection has committed
    /// one stable declaration, a unique constituent carrying that declaration is the receiver view
    /// checked FIR records for its dispatch conversion.
    pub(super) fn selected_intersection_member_receiver(
        &self,
        receiver: Ty,
        selected: &crate::libraries::FunctionInfo,
    ) -> Option<Ty> {
        let Ty::Intersection(bounds) = receiver.non_null() else {
            return None;
        };
        let matching = bounds
            .iter()
            .copied()
            .filter(|bound| {
                self.stable_receiver_callables(*bound, &selected.callable.name)
                    .functions()
                    .iter()
                    .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
                    .any(|candidate| same_callable_declaration(candidate, selected))
            })
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [bound] => Some(*bound),
            // A synthetic intersection override can own the selected candidate even though the
            // declaration identity exposed by an individual constituent differs. Selection has
            // already committed at this point; use the complete candidate families only to recover
            // the unique concrete receiver view, never to repeat overload selection.
            _ => self.intersection_member_receiver(bounds, &selected.callable.name),
        }
    }

    fn intersection_member_receiver(&self, bounds: &[Ty], name: &str) -> Option<Ty> {
        if bounds.len() < 2 {
            return None;
        }
        let member_families = bounds
            .iter()
            .copied()
            .map(|bound| {
                let members = self
                    .stable_receiver_callables(bound, name)
                    .functions()
                    .iter()
                    .filter(|candidate| candidate.kind == crate::libraries::FnKind::Member)
                    .cloned()
                    .collect::<Vec<_>>();
                (bound, members)
            })
            .filter(|(_, members)| !members.is_empty())
            .collect::<Vec<_>>();
        if let [(bound, _)] = member_families.as_slice() {
            return Some(*bound);
        }
        let single = member_families
            .iter()
            .filter_map(|(bound, members)| match members.as_slice() {
                [member] => Some((*bound, member)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if single.len() == member_families.len() && single.len() > 1 {
            let same_parameters = single.windows(2).all(|pair| {
                pair[0].1.semantic_params() == pair[1].1.semantic_params()
                    && pair[0].1.context_count == pair[1].1.context_count
            });
            if same_parameters {
                let context = crate::assignable::TyCtx::new();
                let winners = single
                    .iter()
                    .filter(|(_, candidate)| {
                        single.iter().all(|(_, other)| {
                            crate::assignable::is_subtype(
                                &context,
                                self,
                                candidate.callable.ret,
                                other.callable.ret,
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                if let [winner] = winners.as_slice() {
                    return Some(winner.0);
                }
            }
        }

        self.intersection_property_receiver(bounds, name, false)
    }

    /// Select the concrete receiver view for a synthetic intersection property. Equivalent fake
    /// overrides may occur on several incomparable bounds; after covariant-result filtering they
    /// denote the same property contract, so stable bound order breaks that semantic tie.
    fn intersection_property_receiver(&self, bounds: &[Ty], name: &str, write: bool) -> Option<Ty> {
        if bounds.len() < 2 {
            return None;
        }
        let resolver = self.resolver();
        let properties = bounds
            .iter()
            .copied()
            .filter_map(|bound| {
                let property = resolver.select_member_property(bound, name)?;
                if write
                    && !property
                        .property
                        .as_ref()
                        .is_some_and(|property| property.setter.is_some())
                {
                    return None;
                }
                Some((bound, property.ty))
            })
            .collect::<Vec<_>>();
        if let [(bound, _)] = properties.as_slice() {
            return Some(*bound);
        }
        let context = crate::assignable::TyCtx::new();
        let winners = properties
            .iter()
            .filter(|(_, candidate)| {
                properties.iter().all(|(_, other)| {
                    crate::assignable::is_subtype(&context, self, *candidate, *other)
                })
            })
            .collect::<Vec<_>>();
        match winners.as_slice() {
            [winner] => Some(winner.0),
            [] => None,
            _ if winners.windows(2).all(|pair| pair[0].1 == pair[1].1) => winners
                .into_iter()
                .map(|winner| winner.0)
                .min_by_key(|bound| bound.source_name()),
            _ => None,
        }
    }
}

fn same_callable_declaration(
    left: &crate::libraries::FunctionInfo,
    right: &crate::libraries::FunctionInfo,
) -> bool {
    if let (Some(left), Some(right)) = (left.stable_declaration, right.stable_declaration) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (left.source_member, right.source_member) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (
        left.callable.external_identity,
        right.callable.external_identity,
    ) {
        return left == right;
    }
    left.kind == right.kind
        && left.callable.owner == right.callable.owner
        && left.callable.name == right.callable.name
        && left.callable.descriptor == right.callable.descriptor
}
