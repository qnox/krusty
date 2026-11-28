//! Specialization, ordering, and visibility of package and extension property candidates.

use super::*;

pub(super) fn specialize_property(
    source: &dyn SymbolSource,
    mut property: PropertyInfo,
    receiver: Ty,
) -> PropertyInfo {
    let generic = property.getter.generic_sig.clone();
    let mut binds = GSigBinds::default();
    if let Some(declared_receiver) = property.receiver {
        unify_receiver_from_symbols(source, declared_receiver, receiver, &mut binds);
    }
    property.receiver = property.receiver.map(|declared| {
        instantiate_slot(
            source,
            generic.as_deref(),
            declared,
            &binds,
            TypePosition::In,
            UnboundSpecialization::UseUpperBound,
        )
    });
    property.ty = instantiate_slot(
        source,
        generic.as_deref(),
        property.ty,
        &binds,
        TypePosition::Out,
        UnboundSpecialization::UseUpperBound,
    );
    property.getter.params = property
        .getter
        .params
        .iter()
        .map(|parameter| {
            instantiate_slot(
                source,
                generic.as_deref(),
                *parameter,
                &binds,
                TypePosition::In,
                UnboundSpecialization::UseUpperBound,
            )
        })
        .collect();
    property.getter.ret = property.ty;
    if let Some(setter) = property.setter.as_mut() {
        setter.params = setter
            .params
            .iter()
            .map(|ty| {
                instantiate_slot(
                    source,
                    generic.as_deref(),
                    *ty,
                    &binds,
                    TypePosition::In,
                    UnboundSpecialization::UseUpperBound,
                )
            })
            .collect();
    }
    property
}

/// Reduce equal receiver-MRO extension-property candidates to the most specific ones. Generic
/// bounds decide first; the receiver, the property's only specificity position, then goes through
/// the shared declaration specificity, whose tie goes to a non-generic declaration over a generic
/// one as for every other callable family: `PipelineContext<*, PipelineCall>.call` beats
/// `<C> PipelineContext<*, C>.call`.
pub(super) fn retain_most_specific_properties<R>(
    source: &dyn SymbolSource,
    candidates: &mut Vec<(R, PropertyInfo)>,
) {
    if candidates.len() > 1 {
        let dominated = (0..candidates.len())
            .filter(|&candidate| {
                (0..candidates.len()).any(|other| {
                    candidate != other
                        && generic_property_more_specific(
                            source,
                            &candidates[other].1,
                            &candidates[candidate].1,
                        )
                })
            })
            .collect::<crate::fx_hash::HashSet<_>>();
        let mut index = 0;
        candidates.retain(|_| {
            let keep = !dominated.contains(&index);
            index += 1;
            keep
        });
    }
    retain_most_specific_declarations(source, None, candidates, |(_, property)| {
        (
            property.receiver,
            &[][..],
            !property.formals.is_empty(),
            None,
        )
    });
}

/// Declaration-site specificity for equal receiver-MRO extension-property candidates. Receiver
/// inference establishes applicability first; this comparison then prefers the declaration whose
/// generic bounds form a strict subtype of the other's after alpha-renaming its formals.
fn generic_property_more_specific(
    source: &dyn SymbolSource,
    left: &PropertyInfo,
    right: &PropertyInfo,
) -> bool {
    let (Some(left), Some(right)) = (
        left.getter.generic_sig.as_deref(),
        right.getter.generic_sig.as_deref(),
    ) else {
        return false;
    };
    if left.formals.len() != right.formals.len()
        || left.formal_bounds.len() != right.formal_bounds.len()
    {
        return false;
    }
    let rename = left
        .formals
        .iter()
        .zip(&right.formals)
        .enumerate()
        .map(|(index, (left_formal, right_formal))| {
            let bound = right
                .formal_bounds
                .get(index)
                .and_then(|bounds| bounds.first())
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            (
                crate::types::TyParamName::from(left_formal),
                Ty::ty_param(right_formal, bound),
            )
        })
        .collect::<GSigBinds>();
    if left
        .receiver
        .map(|receiver| ty_subst_keep_unbound(receiver, &rename))
        != right.receiver
    {
        return false;
    }
    let left_bounds = left
        .formal_bounds
        .iter()
        .map(|bounds| {
            bounds
                .iter()
                .copied()
                .map(|bound| ty_subst_keep_unbound(bound, &rename))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let left_at_least_as_specific =
        left_bounds
            .iter()
            .zip(&right.formal_bounds)
            .all(|(left_bounds, right_bounds)| {
                right_bounds.iter().all(|right_bound| {
                    left_bounds
                        .iter()
                        .any(|left_bound| resolution_subtype(source, *left_bound, *right_bound))
                })
            });
    let right_at_least_as_specific =
        left_bounds
            .iter()
            .zip(&right.formal_bounds)
            .all(|(left_bounds, right_bounds)| {
                left_bounds.iter().all(|left_bound| {
                    right_bounds
                        .iter()
                        .any(|right_bound| resolution_subtype(source, *right_bound, *left_bound))
                })
            });
    left_at_least_as_specific && !right_at_least_as_specific
}

/// Source visibility for package-level properties, applied BEFORE ambiguity/receiver ranking. A JVM
/// `internal`/private declaration may still have a public bytecode accessor, so accessor flags alone
/// must never make it callable from another module. Module-origin facts remain governed by the
/// module's file-aware [`SymbolSource`] overlay (which admits same-file private and hides sibling
/// private); applying a second, file-blind filter here would incorrectly reject the former.
pub(super) fn source_property_visible(
    platform: &dyn SemanticPlatform,
    property: &PropertyInfo,
) -> bool {
    property.visibility == Visibility::Public
        || matches!(property.getter.origin, Origin::Module { .. })
        || (property.visibility == Visibility::Internal
            && platform
                .dependency_access_policy()
                .internal_callable_accessible(property.getter.external_identity, property.owner))
}

impl SymbolResolver<'_> {
    /// The extension property `name` a family of candidates selects for `receiver`: the nearest
    /// receiver rank among the visible, readable or writable candidates, then the most specific.
    pub(super) fn select_extension_property_from_callables<'p>(
        &self,
        receiver: Ty,
        name: &str,
        access: PropertyAccess,
        properties: impl IntoIterator<Item = &'p PropertyInfo>,
    ) -> Result<Option<PropertyInfo>, AmbiguousExtensionProperty> {
        let mut properties = properties.into_iter().peekable();
        if properties.peek().is_none() {
            return Ok(None);
        }
        let receiver_mro = self.receiver_mro(receiver);
        let mut candidates = properties
            .filter(|property| property.kind == PropKind::Extension)
            .filter(|property| match access {
                PropertyAccess::Read => property.readable(),
                PropertyAccess::Write => property.write_selectable(),
            })
            .filter(|property| self.property_visible(property))
            .filter(|property| {
                generic_bounds_admit(
                    &self.src,
                    property.getter.generic_sig.as_deref(),
                    receiver,
                    &[],
                    &[],
                )
            })
            .filter_map(|property| {
                let declared = property.receiver?;
                let rank = receiver_mro.rank(&self.src, declared);
                crate::trace_compiler!(
                    "resolve",
                    "extension property candidate name={name} receiver={receiver:?} declared={declared:?} setter={} source={:?} rank={rank:?}",
                    property.setter.is_some(),
                    property.source_key,
                );
                rank.map(|rank| (rank, property.clone()))
            })
            .collect::<Vec<_>>();
        let Some(nearest) = candidates.iter().map(|(rank, _)| *rank).min() else {
            return Ok(None);
        };
        candidates.retain(|(rank, _)| *rank == nearest);
        retain_most_specific_properties(&self.src, &mut candidates);
        match candidates.as_mut_slice() {
            [(_, property)] => Ok(Some(specialize_property(
                &self.src,
                property.clone(),
                receiver,
            ))),
            _ => Err(AmbiguousExtensionProperty),
        }
    }
}
