//! Receiver views committed after selection from a semantic intersection.

use super::*;
use crate::types::TyKind;

impl Checker<'_> {
    /// Project the current implicit receiver's flow intersection onto the constituent that owns the
    /// member a bare call names, as [`Self::flow_intersection_member_receiver`] does for `this.name`.
    pub(super) fn project_implicit_receiver_for_member(
        &self,
        scope: &CheckerScope<'_>,
        receivers: &mut [ImplicitReceiver],
        name: &str,
        expected: Option<Ty>,
    ) {
        let Some(path) = self.current_receiver_path(scope) else {
            return;
        };
        let bounds = self.lookup_intersection_narrowing(scope, &path);
        let Some(projected) = self.intersection_member_receiver(&bounds, name) else {
            return;
        };
        // kotlinc completes a call that must satisfy a proper expected type apart from the
        // builder's variables, so a result reached through the smart-cast receiver no longer
        // carries them.
        if self.postponed_call_mentions(projected)
            && expected.is_some_and(|expected| !self.postponed_call_mentions(expected))
        {
            return;
        }
        if let Some(current) = receivers.iter_mut().find(|receiver| receiver.current) {
            current.ty = projected;
        }
    }

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
        let path = self.flow_read_path(receiver)?;
        let bounds = self.lookup_intersection_narrowing(scope, &path);
        self.intersection_member_receiver(&bounds, name)
    }

    /// The result of `selected` called through a flow intersection. kotlinc's intersection
    /// override returns the most specific result among the constituents' members of the same
    /// shape, whichever constituent dispatches the call: on `Option<Pair<A, B>> & Some<*>`,
    /// `get()` dispatches through `Some<*>` but returns `Pair<A, B>`, not `Some<*>`'s `Any?`.
    /// `None` without such an override, or when its result is the selected one.
    pub(super) fn flow_intersection_member_result(
        &self,
        scope: &CheckerScope<'_>,
        receiver: ExprId,
        selected: &crate::libraries::FunctionInfo,
    ) -> Option<Ty> {
        // A member's own type arguments are inferred at the call and already applied to the
        // selected result; the constituents' members carry only their receiver's substitution.
        if selected
            .generic_sig
            .as_ref()
            .is_some_and(|signature| !signature.formals.is_empty())
        {
            return None;
        }
        let path = self.flow_read_path(receiver)?;
        let bounds = self.lookup_intersection_narrowing(scope, &path);
        if bounds.len() < 2 {
            return None;
        }
        let families = self.intersection_member_families(&bounds, &selected.callable.name);
        let (_, result) = self.intersection_override(&families)?;
        (result != selected.callable.ret).then_some(result)
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
        let path = self.flow_read_path(receiver)?;
        let bounds = self.lookup_intersection_narrowing(scope, &path);
        self.intersection_property_receiver(&bounds, name, write)
    }

    /// The receiver view of a bare property `name` read through an implicit receiver whose flow
    /// type is an intersection (`this is B` on an incomparable declaration): the same projection
    /// an explicit receiver's flow intersection gets, so checked FIR casts `this` to the
    /// constituent that owns the selected property.
    pub(super) fn implicit_intersection_property_view(
        &self,
        receiver: ImplicitReceiver,
        name: &str,
    ) -> ImplicitReceiver {
        self.implicit_intersection_view(receiver, |bounds| {
            self.intersection_property_receiver(bounds, name, false)
        })
    }

    /// [`Self::implicit_intersection_property_view`] for a bare call of the member `name`.
    pub(super) fn implicit_intersection_call_view(
        &self,
        receiver: ImplicitReceiver,
        name: &str,
    ) -> ImplicitReceiver {
        self.implicit_intersection_view(receiver, |bounds| {
            self.intersection_member_receiver(bounds, name)
        })
    }

    /// A nullable intersection stays whole: a bare member through it is an unsafe call.
    fn implicit_intersection_view(
        &self,
        receiver: ImplicitReceiver,
        project: impl FnOnce(&[Ty]) -> Option<Ty>,
    ) -> ImplicitReceiver {
        if receiver.ty.is_nullable() {
            return receiver;
        }
        let TyKind::Intersection(bounds) = receiver.ty.kind() else {
            return receiver;
        };
        project(bounds.as_slice()).map_or(receiver, |ty| ImplicitReceiver { ty, ..receiver })
    }

    /// The path whose flow type `receiver` reads. A cast `(x as T)` denotes `x`'s path, but its
    /// value has the cast's target type, never the flow intersection proven for `x`.
    fn flow_read_path(&self, receiver: ExprId) -> Option<NarrowPath> {
        if matches!(self.file.expr(receiver), Expr::As { .. }) {
            return None;
        }
        self.expr_access_path(receiver)
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
        let TyKind::Intersection(bounds) = receiver.non_null().kind() else {
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
                    .any(|candidate| {
                        crate::symbol_resolver::same_callable_declaration(candidate, selected)
                    })
            })
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [bound] => Some(*bound),
            // A synthetic intersection override can own the selected candidate even though the
            // declaration identity exposed by an individual constituent differs. Selection has
            // already committed at this point; use the complete candidate families only to recover
            // the unique concrete receiver view, never to repeat overload selection.
            _ => self.intersection_member_receiver(bounds.as_slice(), &selected.callable.name),
        }
    }

    fn intersection_member_receiver(&self, bounds: &[Ty], name: &str) -> Option<Ty> {
        if bounds.len() < 2 {
            return None;
        }
        let member_families = self.intersection_member_families(bounds, name);
        if let [(bound, _)] = member_families.as_slice() {
            return Some(*bound);
        }
        if let Some(overriding) =
            self.overriding_constituent(member_families.iter().map(|(bound, _)| *bound))
        {
            return Some(overriding);
        }
        if let Some((bound, _)) = self.intersection_override(&member_families) {
            return Some(bound);
        }

        self.intersection_property_receiver(bounds, name, false)
    }

    /// The member functions `name` each constituent of `bounds` declares, for the constituents
    /// that declare any.
    fn intersection_member_families(
        &self,
        bounds: &[Ty],
        name: &str,
    ) -> Vec<(Ty, Vec<crate::libraries::FunctionInfo>)> {
        bounds
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
            .collect()
    }

    /// Kotlin's synthetic intersection override over member families: when at least two
    /// constituents each declare exactly one member of the same parameter shape, the override has
    /// the uniquely most specific result. Returns the constituent declaring that result, and the
    /// result itself.
    fn intersection_override(
        &self,
        families: &[(Ty, Vec<crate::libraries::FunctionInfo>)],
    ) -> Option<(Ty, Ty)> {
        let single = families
            .iter()
            .filter_map(|(bound, members)| match members.as_slice() {
                [member] => Some((*bound, member)),
                _ => None,
            })
            .collect::<Vec<_>>();
        if single.len() != families.len() || single.len() < 2 {
            return None;
        }
        let same_parameters = single.windows(2).all(|pair| {
            pair[0].1.semantic_params() == pair[1].1.semantic_params()
                && pair[0].1.context_count == pair[1].1.context_count
        });
        if !same_parameters {
            return None;
        }
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
        match winners.as_slice() {
            [(bound, member)] => Some((*bound, member.callable.ret)),
            _ => None,
        }
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
        if let Some(overriding) =
            self.overriding_constituent(properties.iter().map(|(bound, _)| *bound))
        {
            return Some(overriding);
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
            _ if winners.windows(2).all(|pair| pair[0].1 == pair[1].1) => winners
                .into_iter()
                .map(|winner| winner.0)
                .min_by_key(|bound| bound.source_name()),
            _ => None,
        }
    }

    /// The constituent whose classifier inherits every other contributor of a member. The member
    /// is then one declaration overridden down the class hierarchy (`Source<T>.value` beside
    /// `IntBox.value`, `Buildee<T>.getTypeVariable()` beside its fake override in
    /// `DerivedBuildee<*>`), and kotlinc dispatches it through that overriding constituent
    /// whatever the results' type arguments.
    fn overriding_constituent(&self, contributors: impl Iterator<Item = Ty> + Clone) -> Option<Ty> {
        let mut overriding = contributors.clone().filter(|bound| {
            contributors.clone().all(|other| {
                other == *bound
                    || crate::assignable::applied_supertype(
                        self,
                        bound.non_null(),
                        other.non_null(),
                    )
                    .is_some()
            })
        });
        let found = overriding.next()?;
        overriding.next().is_none().then_some(found)
    }
}
