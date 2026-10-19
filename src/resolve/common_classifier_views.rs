//! Most-specific common classifier views of expression types.
//!
//! A common supertype can have several incomparable classifier facets. Call resolution retains
//! those facets separately at the top level, while generic arguments use the shared n-ary,
//! variance-aware common-supertype solver so an early nested intersection is not erased.

use crate::fx_hash::{HashMap, HashSet};
use crate::types::Ty;

use super::{common_result_nullability, Checker};

impl Checker<'_> {
    /// Most-specific common classifier intersection for any number of lower constraints. Each
    /// common owner is joined with all of its applied type arguments before dominance filtering,
    /// so `C<String>` and `D<String>` preserve `BBB<String>` beside an unrelated marker interface.
    pub(super) fn minimal_common_classifier_views_many(&self, types: &[Ty]) -> Vec<Ty> {
        let edges = types.iter().copied().map(|ty| vec![ty]).collect::<Vec<_>>();
        self.minimal_common_classifier_views_for_edges(&edges)
    }

    /// Most-specific common classifier intersection across flow edges. One edge can itself carry
    /// an intersection (`A & B`); its classifier closure is the union of those proven facets, and
    /// a classifier survives only when every edge has that view.
    pub(super) fn minimal_common_classifier_views_for_edges(&self, edges: &[Vec<Ty>]) -> Vec<Ty> {
        let Some(first) = edges.first() else {
            return Vec::new();
        };
        let source = self.fed_source();
        let edge_hierarchy = |facets: &[Ty]| {
            let mut seen = HashSet::default();
            facets
                .iter()
                .flat_map(|ty| crate::symbol_resolver::applied_hierarchy(&source, ty.non_null()))
                .filter_map(|(owner, applied, _)| seen.insert(owner).then_some((owner, applied)))
                .collect::<Vec<_>>()
        };
        let mut common = edge_hierarchy(first)
            .into_iter()
            .map(|(owner, applied)| (owner, vec![applied]))
            .collect::<Vec<_>>();
        for edge in &edges[1..] {
            let next = edge_hierarchy(edge).into_iter().collect::<HashMap<_, _>>();
            common.retain_mut(|(owner, applications)| {
                let Some(applied) = next.get(owner).copied() else {
                    return false;
                };
                applications.push(applied);
                true
            });
        }
        let dominated = common
            .iter()
            .flat_map(|(owner, _)| {
                crate::symbol_resolver::applied_hierarchy(&source, Ty::obj_name(*owner))
                    .into_iter()
                    .skip(1)
                    .map(|(ancestor, _, _)| ancestor)
            })
            .collect::<HashSet<_>>();
        let mut views = common
            .into_iter()
            .filter(|(owner, _)| !dominated.contains(owner))
            .filter_map(|(owner, applications)| {
                // A pairwise fold can erase a nested intersection before a later argument removes
                // one of its facets (`Source<A & B>`, then `Source<A>`). The inference solver owns
                // this n-ary algebra and retains nested intersections explicitly.
                let joined = crate::symbol_resolver::common_super_types(&source, &applications);
                matches!(joined.non_null(), Ty::Obj(joined_owner, _) if joined_owner == owner)
                    .then_some(joined)
            })
            .map(|joined| {
                edges.iter().flatten().copied().fold(joined, |result, ty| {
                    common_result_nullability(result, ty, joined)
                })
            })
            .collect::<Vec<_>>();
        let mut seen = HashSet::default();
        views.retain(|view| seen.insert(*view));
        views
    }
}
