//! Common supertype of inferred lower bounds.
//!
//! Kotlin does not erase `Inv<A>` and `Inv<B>` to a raw `Inv` or to `Any`. The classifier stays,
//! and each type argument is joined by its variance: a covariant argument joins directly, and an
//! invariant argument becomes an out-projection of that join (a captured `out` type). Unrelated
//! classifiers join at every most-specific shared supertype, which is an intersection when more
//! than one remains (`A : X, Y` and `B : X, Y` join at `X & Y`).

use std::collections::{HashSet, VecDeque};

use super::super::hierarchy_projection::{direct_supertypes, intersection_components};
use super::super::SourceOracle;
use crate::symbol_source::SymbolSource;
use crate::types::{Ty, TypeName, TypeVariance};

pub(super) fn common_super_type(source: &dyn SymbolSource, left: Ty, right: Ty) -> Ty {
    common_super_types(source, &[left, right])
}

pub(super) fn common_super_types(source: &dyn SymbolSource, bounds: &[Ty]) -> Ty {
    let nullable = bounds.iter().any(|bound| bound.is_nullable());
    let bounds = bounds
        .iter()
        .map(|bound| bound.non_null())
        .collect::<Vec<_>>();
    let result = CommonSupertypeSolver::new(source).common(&bounds);
    if result == Ty::Error {
        return Ty::Error;
    }
    if nullable {
        Ty::nullable(result.non_null())
    } else {
        result
    }
}

struct CommonSupertypeSolver<'a> {
    source: &'a dyn SymbolSource,
    active: Vec<(bool, Vec<Ty>)>,
    active_classifiers: Vec<(TypeName, usize)>,
    completed: Vec<(bool, Vec<Ty>, Ty)>,
    invalid_classifier_shape: bool,
}

impl<'a> CommonSupertypeSolver<'a> {
    fn new(source: &'a dyn SymbolSource) -> Self {
        Self {
            source,
            active: Vec::new(),
            active_classifiers: Vec::new(),
            completed: Vec::new(),
            invalid_classifier_shape: false,
        }
    }

    fn common(&mut self, bounds: &[Ty]) -> Ty {
        let result = self
            .common_at(bounds, false)
            .unwrap_or_else(|| Ty::obj_name(crate::types::wk::any()));
        if self.invalid_classifier_shape {
            Ty::Error
        } else {
            result
        }
    }

    /// Join one unordered constraint set. Returning `None` means this exact join is already active:
    /// the classifier candidate that led back to it is recursive and must not participate in this
    /// round. Other shared classifiers still compete, and `Any` remains the sound result when no
    /// acyclic candidate exists.
    fn common_at(&mut self, bounds: &[Ty], permit_intersection: bool) -> Option<Ty> {
        let mut key = Vec::with_capacity(bounds.len());
        for bound in bounds.iter().copied().map(Ty::non_null) {
            if !key.contains(&bound) {
                key.push(bound);
            }
        }
        let Some((&first, rest)) = key.split_first() else {
            return Some(Ty::obj_name(crate::types::wk::any()));
        };
        if rest.iter().all(|bound| *bound == first) {
            return Some(first);
        }
        if let Some((_, _, result)) = self.completed.iter().find(|(permit, known, _)| {
            *permit == permit_intersection && same_bound_set(known, &key)
        }) {
            return Some(*result);
        }
        if self
            .active
            .iter()
            .any(|(permit, active)| *permit == permit_intersection && same_bound_set(active, &key))
        {
            return None;
        }
        self.active.push((permit_intersection, key.clone()));
        let result = self.common_active(&key, permit_intersection);
        self.active.pop();
        self.completed.push((permit_intersection, key, result));
        Some(result)
    }

    fn common_active(&mut self, bounds: &[Ty], permit_intersection: bool) -> Ty {
        let Some((&first, rest)) = bounds.split_first() else {
            return Ty::obj_name(crate::types::wk::any());
        };
        if rest.iter().all(|bound| *bound == first) {
            return first;
        }
        let oracle = SourceOracle(self.source);
        let context = crate::assignable::TyCtx::new();
        let closures = bounds
            .iter()
            .map(|bound| closure(self.source, *bound))
            .collect::<Vec<_>>();
        let mut classifiers = Vec::new();
        for candidate in &closures[0] {
            let Ty::Obj(classifier, arguments) = candidate else {
                continue;
            };
            if !classifiers.contains(&(*classifier, arguments.len()))
                && closures.iter().skip(1).all(|types| {
                    types.iter().any(|ty| {
                        matches!(ty, Ty::Obj(name, args)
                        if name == classifier && args.len() == arguments.len())
                    })
                })
            {
                classifiers.push((*classifier, arguments.len()));
            }
        }

        // A top-level inference binding must be denotable. Multiple incomparable common views are
        // retained separately by the call-selection intersection contract; putting their synthetic
        // intersection into the binding changes ABI decisions such as a generic vararg's array
        // component. Nested invariant arguments are different: `Inv<A>` plus `Inv<B>` is expressed
        // as `Inv<out (X & Y)>`, so that argument join explicitly permits the intersection.
        let dominated_owners = classifiers
            .iter()
            .flat_map(|(owner, _)| {
                closure(self.source, Ty::obj_name(*owner))
                    .into_iter()
                    .skip(1)
            })
            .filter_map(Ty::obj_internal)
            .collect::<HashSet<_>>();
        classifiers.retain(|(owner, _)| !dominated_owners.contains(owner));
        if !permit_intersection && classifiers.len() > 1 {
            return Ty::obj_name(crate::types::wk::any());
        }

        let mut candidates = Vec::new();
        for (classifier, arity) in classifiers {
            let matching = closures
            .iter()
            .map(|types| {
                types
                    .iter()
                    .copied()
                    .filter(|ty| matches!(ty, Ty::Obj(name, args) if *name == classifier && args.len() == arity))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
            self.collect_classifier_combinations(
                classifier,
                &matching,
                0,
                &mut Vec::new(),
                &mut candidates,
            );
        }
        let mut most_specific = Vec::new();
        for index in 0..candidates.len() {
            let candidate = candidates[index];
            let dominated = candidates.iter().any(|other| {
                other != &candidate
                    && crate::assignable::is_assignable(&context, &oracle, *other, candidate)
                    && !crate::assignable::is_assignable(&context, &oracle, candidate, *other)
            });
            if !dominated {
                most_specific.push(candidate);
            }
        }
        match most_specific.as_slice() {
            [] => Ty::obj("kotlin/Any"),
            [one] => *one,
            many if permit_intersection => Ty::intersection(many),
            _ => Ty::obj("kotlin/Any"),
        }
    }

    fn collect_classifier_combinations(
        &mut self,
        classifier: TypeName,
        matching: &[Vec<Ty>],
        index: usize,
        selected: &mut Vec<Ty>,
        candidates: &mut Vec<Ty>,
    ) {
        if index == matching.len() {
            if let Some(combined) = self.combine_classifier(classifier, selected) {
                if !candidates.contains(&combined) {
                    candidates.push(combined);
                }
            }
            return;
        }
        for ty in &matching[index] {
            selected.push(*ty);
            self.collect_classifier_combinations(
                classifier,
                matching,
                index + 1,
                selected,
                candidates,
            );
            selected.pop();
        }
    }

    fn combine_classifier(&mut self, classifier: TypeName, types: &[Ty]) -> Option<Ty> {
        let size = types.iter().copied().map(type_size).sum();
        if self
            .active_classifiers
            .iter()
            .any(|(active, active_size)| *active == classifier && size >= *active_size)
        {
            return None;
        }
        self.active_classifiers.push((classifier, size));
        let result = self.combine_classifier_inner(classifier, types);
        self.active_classifiers.pop();
        result
    }

    fn combine_classifier_inner(&mut self, classifier: TypeName, types: &[Ty]) -> Option<Ty> {
        let arguments = types
            .iter()
            .map(|ty| match ty {
                Ty::Obj(_, arguments) => arguments,
                _ => &[][..],
            })
            .collect::<Vec<_>>();
        let arity = arguments.first().map_or(0, |arguments| arguments.len());
        if arity == 0 {
            return Some(Ty::obj_name(classifier));
        }
        let Some(shape) = self.source.classifier(classifier) else {
            self.invalid_classifier_shape = true;
            return None;
        };
        let variances = shape.type_param_variances();
        if variances.len() != arity {
            self.invalid_classifier_shape = true;
            return None;
        }
        let mut combined_arguments = Vec::with_capacity(arity);
        for index in 0..arity {
            let projected = arguments
                .iter()
                .map(|arguments| arguments[index])
                .collect::<Vec<_>>();
            if projected.iter().all(|argument| *argument == projected[0]) {
                combined_arguments.push(projected[0]);
                continue;
            }
            let variance = variances[index];
            if variance == TypeVariance::In {
                // A contravariant argument contributes no readable lower bound. Computing its
                // common supertype is both semantically unused and recursively re-enters F-bounded
                // declarations such as Comparable<in T>.
                combined_arguments.push(Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any"))));
                continue;
            }
            let readable = projected
                .iter()
                .map(|argument| argument.projection_read_ty().non_null())
                .collect::<Vec<_>>();
            let joined = self.common_at(&readable, true)?;
            combined_arguments.push(if variance == TypeVariance::Out {
                joined
            } else {
                Ty::out_projection(joined)
            });
        }
        Some(Ty::obj_args_name(classifier, &combined_arguments))
    }
}

fn type_size(ty: Ty) -> usize {
    match ty {
        Ty::Obj(_, arguments) => 1 + arguments.iter().copied().map(type_size).sum::<usize>(),
        Ty::Fun(signature) => {
            1 + signature
                .params
                .iter()
                .copied()
                .map(type_size)
                .sum::<usize>()
                + type_size(signature.ret)
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner)
        | Ty::StarProjection(inner)
        | Ty::DefinitelyNotNull(inner) => 1 + type_size(*inner),
        Ty::TyParam(_, bound) => 1 + type_size(*bound),
        Ty::Intersection(parts) => 1 + parts.iter().copied().map(type_size).sum::<usize>(),
        _ => 1,
    }
}

fn same_bound_set(left: &[Ty], right: &[Ty]) -> bool {
    left.len() == right.len() && left.iter().all(|bound| right.contains(bound))
}

/// The classifier a reified operation records. An intersection is not a runtime class. When every
/// component is a resolved classifier, the operation uses their single common supertype (`Any`
/// when they share none, that shared classifier when they do). An unresolved component keeps the
/// checked intersection: a missing hierarchy is not evidence that the components meet only at `Any`.
pub(crate) fn reified_runtime_type(
    ty: Ty,
    direct_supertypes: &dyn Fn(TypeName) -> Option<Vec<TypeName>>,
) -> Ty {
    let nullable = ty.is_nullable();
    let inner = ty.non_null().projection_read_ty().non_null();
    let Ty::Intersection(parts) = inner else {
        return ty;
    };
    let Some(approximated) = approximate_reified_intersection(parts, direct_supertypes) else {
        return ty;
    };
    if nullable {
        Ty::nullable(approximated)
    } else {
        approximated
    }
}

fn approximate_reified_intersection(
    parts: &[Ty],
    direct_supertypes: &dyn Fn(TypeName) -> Option<Vec<TypeName>>,
) -> Option<Ty> {
    if parts.is_empty() {
        return None;
    }
    let mut names = Vec::with_capacity(parts.len());
    for part in parts {
        names.push(part.non_null().obj_internal()?);
    }
    Some(Ty::obj_name(single_reified_supertype(
        &names,
        direct_supertypes,
    )?))
}

fn single_reified_supertype(
    components: &[TypeName],
    direct_supertypes: &dyn Fn(TypeName) -> Option<Vec<TypeName>>,
) -> Option<TypeName> {
    let mut closures = Vec::with_capacity(components.len());
    for name in components {
        closures.push(supertype_closure(*name, direct_supertypes)?);
    }
    let mut common = closures.first().cloned().unwrap_or_default();
    for closure in closures.iter().skip(1) {
        common.retain(|name| closure.contains(name));
    }
    let any = crate::types::wk::any();
    loop {
        if common.is_empty() {
            return Some(any);
        }
        let most = common
            .iter()
            .copied()
            .filter(|name| {
                !common.iter().any(|other| {
                    other != name
                        && supertype_closure(*other, direct_supertypes)
                            .is_some_and(|closure| closure.contains(name))
                })
            })
            .collect::<Vec<_>>();
        if most.len() == 1 {
            return Some(most[0]);
        }
        if most.is_empty() || most.len() == common.len() {
            return Some(any);
        }
        for name in most {
            common.remove(&name);
        }
    }
}

fn supertype_closure(
    name: TypeName,
    direct_supertypes: &dyn Fn(TypeName) -> Option<Vec<TypeName>>,
) -> Option<HashSet<TypeName>> {
    let mut seen = HashSet::new();
    let mut pending = vec![name];
    let any = crate::types::wk::any();
    while let Some(current) = pending.pop() {
        if !seen.insert(current) {
            continue;
        }
        if current == any {
            continue;
        }
        pending.extend(direct_supertypes(current)?);
        pending.push(any);
    }
    Some(seen)
}

fn closure(source: &dyn SymbolSource, root: Ty) -> Vec<Ty> {
    let mut queue = VecDeque::from([root.non_null()]);
    let mut seen = HashSet::new();
    let mut types = Vec::new();
    let any = Ty::obj_name(crate::types::wk::any());
    while let Some(current) = queue.pop_front() {
        if let Some(parts) = intersection_components(current) {
            queue.extend(parts.iter().copied().map(Ty::non_null));
            continue;
        }
        if !seen.insert(current) {
            continue;
        }
        types.push(current);
        queue.extend(
            direct_supertypes(source, current)
                .into_iter()
                .map(Ty::non_null),
        );
        if current != any {
            queue.push_back(any);
        }
    }
    types
}

#[cfg(test)]
mod tests {
    use super::{common_super_type, common_super_types, reified_runtime_type};
    use crate::libraries::{Callables, ClassifierDeclaration, LibraryType, ResolvedSymbols};
    use crate::symbol_source::{SymbolNamespace, SymbolSource};
    use crate::types::{type_name, Ty, TypeParameters, TypeVariance};
    use std::rc::Rc;
    use std::sync::Arc;

    struct EmptySource;

    impl SymbolSource for EmptySource {}

    struct GenericSource {
        variance: TypeVariance,
    }

    impl SymbolSource for GenericSource {
        fn symbols(&self, namespace: SymbolNamespace, name: &str) -> Rc<ResolvedSymbols> {
            let identity = namespace.existing_classifier(name);
            let classifier = identity
                .filter(|identity| identity.matches("demo/Inv"))
                .map(|_| {
                    let mut classifier = LibraryType::declaration_header();
                    classifier.type_parameters = TypeParameters::new(
                        vec!["T".to_string()],
                        vec![Vec::new()],
                        vec![self.variance],
                    );
                    Arc::new(classifier)
                });
            Rc::new(ResolvedSymbols {
                builtin_classifier: false,
                classifier_name: classifier.as_ref().and(identity),
                classifier_declaration: classifier
                    .as_ref()
                    .and(identity)
                    .map(ClassifierDeclaration::Ordinary),
                classifier,
                callables: Callables::None,
                importable_declaration: false,
            })
        }
    }

    #[test]
    fn a_deep_invariant_join_is_not_operand_order_dependent() {
        let mut left = Ty::obj("demo/Left");
        let mut right = Ty::obj("demo/Right");
        for _ in 0..24 {
            left = Ty::obj_args("demo/Inv", &[left]);
            right = Ty::obj_args("demo/Inv", &[right]);
        }

        let source = GenericSource {
            variance: TypeVariance::Invariant,
        };
        let left_first = common_super_type(&source, left, right);
        let right_first = common_super_type(&source, right, left);
        assert_eq!(left_first, right_first);
        assert_ne!(left_first, left);
        assert_ne!(left_first, right);
    }

    #[test]
    fn generic_join_uses_the_declared_variance() {
        let left = Ty::obj_args("demo/Inv", &[Ty::obj("demo/Left")]);
        let right = Ty::obj_args("demo/Inv", &[Ty::obj("demo/Right")]);
        let any = Ty::obj("kotlin/Any");

        assert_eq!(
            common_super_type(
                &GenericSource {
                    variance: TypeVariance::Invariant,
                },
                left,
                right,
            ),
            Ty::obj_args("demo/Inv", &[Ty::out_projection(any)])
        );
        assert_eq!(
            common_super_type(
                &GenericSource {
                    variance: TypeVariance::Out,
                },
                left,
                right,
            ),
            Ty::obj_args("demo/Inv", &[any])
        );
        assert_eq!(
            common_super_type(
                &GenericSource {
                    variance: TypeVariance::In,
                },
                left,
                right,
            ),
            Ty::obj_args("demo/Inv", &[Ty::star_projection(Ty::nullable(any))])
        );
    }

    #[test]
    fn a_missing_generic_declaration_is_not_treated_as_invariant() {
        assert_eq!(
            common_super_type(
                &EmptySource,
                Ty::obj_args("demo/Inv", &[Ty::obj("demo/Left")]),
                Ty::obj_args("demo/Inv", &[Ty::obj("demo/Right")]),
            ),
            Ty::Error
        );
    }

    #[test]
    fn an_n_ary_join_does_not_discard_the_original_lower_bounds() {
        let left = Ty::obj("demo/Left");
        let middle = Ty::obj("demo/Middle");
        let right = Ty::obj("demo/Right");
        let forward = common_super_types(&EmptySource, &[left, middle, right]);
        let reverse = common_super_types(&EmptySource, &[right, middle, left]);
        assert_eq!(forward, reverse);
    }

    #[test]
    fn an_intersection_reifies_as_its_single_common_supertype() {
        let x = type_name("demo/X");
        let y = type_name("demo/Y");
        let z = type_name("demo/Z");
        let intersection = Ty::intersection(&[Ty::obj_name(x), Ty::obj_name(y)]);
        assert_eq!(
            reified_runtime_type(intersection, &|_| None),
            intersection,
            "a missing classifier hierarchy must not become Any"
        );
        assert_eq!(
            reified_runtime_type(intersection, &|_| Some(Vec::new())),
            Ty::obj_name(crate::types::wk::any())
        );
        assert_eq!(
            reified_runtime_type(intersection, &|name| {
                if name == x || name == y {
                    Some(vec![z])
                } else if name == z {
                    Some(Vec::new())
                } else {
                    None
                }
            }),
            Ty::obj_name(z)
        );
    }
}
