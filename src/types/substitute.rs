//! Type-parameter substitution. Callers supply a lookup; the walk never builds a map of its own.

use crate::fx_hash::HashMap;
use crate::types::TyKind;

use super::{intern, Ty, TyParamName};

/// A substitution of type parameters, keyed by their semantic identity.
pub(crate) type TyParamBindings = HashMap<TyParamName, Ty>;

fn compose_in_projection(inner: Ty) -> Ty {
    match inner.kind() {
        TyKind::StarProjection(_) | TyKind::InProjection(_) => inner,
        _ => Ty::in_projection(inner),
    }
}

fn compose_out_projection(inner: Ty) -> Ty {
    match inner.kind() {
        TyKind::StarProjection(_) | TyKind::OutProjection(_) => inner,
        _ => Ty::out_projection(inner),
    }
}

/// Substitute semantic type parameters throughout one type shape. This belongs to the type model:
/// providers, overload selection, checking, and lowering all consume the same transformation.
fn substitute_type_parameters<F, P>(
    ty: Ty,
    lookup: F,
    preserve_unbound: P,
    enforce_bound_nullability: bool,
) -> Ty
where
    F: Fn(&str) -> Option<Ty> + Copy,
    P: Fn(&str) -> bool + Copy,
{
    match ty.kind() {
        // A Java raw receiver binds each formal to its erasure: its member reads it as erased.
        TyKind::TyParam(name, bound) => lookup(name.as_str())
            .map(Ty::raw_binding_read)
            .map(|binding| {
                if !enforce_bound_nullability || bound.upper_bound_admits_null() {
                    binding
                } else {
                    binding.non_null()
                }
            })
            .unwrap_or_else(|| {
                if preserve_unbound(name.as_str()) {
                    ty
                } else {
                    bound.non_null()
                }
            }),
        TyKind::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| {
                    substitute_type_parameters(
                        *parameter,
                        lookup,
                        preserve_unbound,
                        enforce_bound_nullability,
                    )
                })
                .collect(),
            substitute_type_parameters(
                signature.ret,
                lookup,
                preserve_unbound,
                enforce_bound_nullability,
            ),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        // An unbound-preserving walk is identity-only and keeps the provenance wrapper. A real
        // binding consumes the occurrence: concrete nullable types become non-null, while a
        // nullable-bounded symbolic binding becomes that caller parameter's own `T & Any`.
        TyKind::DefinitelyNotNull(inner) => {
            let other = inner;
            match other.kind() {
                TyKind::TyParam(name, bound) => {
                    match lookup(name.as_str()).map(Ty::raw_binding_read) {
                        Some(binding) => binding.contributed_through_nullable_formal(),
                        None if preserve_unbound(name.as_str()) => ty,
                        None => bound.contributed_through_nullable_formal(),
                    }
                }
                _ => substitute_type_parameters(
                    other,
                    lookup,
                    preserve_unbound,
                    enforce_bound_nullability,
                )
                .contributed_through_nullable_formal(),
            }
        }
        TyKind::Nullable(inner) => Ty::nullable(substitute_type_parameters(
            inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        TyKind::PlatformNullable(inner) => Ty::platform_nullable(substitute_type_parameters(
            inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        TyKind::MutabilityFlexible(inner) => Ty::mutability_flexible(substitute_type_parameters(
            inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        TyKind::InProjection(inner) => compose_in_projection(substitute_type_parameters(
            inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        TyKind::OutProjection(inner) => compose_out_projection(substitute_type_parameters(
            inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        TyKind::StarProjection(inner) => Ty::star_projection(substitute_type_parameters(
            inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        TyKind::RawProjection(inner) => Ty::raw_projection(substitute_type_parameters(
            inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        TyKind::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            name,
            &arguments
                .iter()
                .map(|argument| {
                    substitute_type_parameters(
                        *argument,
                        lookup,
                        preserve_unbound,
                        enforce_bound_nullability,
                    )
                })
                .collect::<Vec<_>>(),
        ),
        TyKind::Intersection(parts) => Ty::intersection(
            &parts
                .iter()
                .map(|part| {
                    substitute_type_parameters(
                        *part,
                        lookup,
                        preserve_unbound,
                        enforce_bound_nullability,
                    )
                })
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    }
}

/// A type-parameter binding map. The frontend keys bindings by [`TyParamName`]; lowering and
/// backends still key theirs by the identity's text until they move to the typed key.
pub(crate) trait TyParamKey: std::borrow::Borrow<str> + Eq + std::hash::Hash {}

impl TyParamKey for TyParamName {}
impl TyParamKey for String {}

fn recorded_binding<K: TyParamKey>(bindings: &HashMap<K, Ty>, name: &str) -> Option<Ty> {
    bindings.get(name).copied()
}

pub(crate) fn ty_subst<K: TyParamKey>(ty: Ty, bindings: &HashMap<K, Ty>) -> Ty {
    substitute_type_parameters(ty, |name| recorded_binding(bindings, name), |_| false, true)
}

/// Apply a typealias template to its use-site arguments.
///
/// A projection belongs to the alias argument. When that formal occurs directly as a structural
/// function parameter/result, the function component consumes the projection and keeps its
/// underlying semantic type (`typealias F<T> = () -> T; F<*>` returns the star's `Any?` bound).
/// Projections below a classifier remain written generic arguments and must survive.
pub(crate) fn ty_subst_alias_expansion<K: TyParamKey>(ty: Ty, bindings: &HashMap<K, Ty>) -> Ty {
    fn normalize(ty: Ty) -> Ty {
        match ty.kind() {
            TyKind::Fun(signature) => Ty::fun_with_shape(
                signature
                    .params
                    .iter()
                    .map(|parameter| function_component(normalize(*parameter)))
                    .collect(),
                function_component(normalize(signature.ret)),
                signature.context_count,
                signature.has_receiver,
                signature.suspend,
            ),
            TyKind::Nullable(inner) => Ty::nullable(normalize(inner)),
            TyKind::PlatformNullable(inner) => Ty::platform_nullable(normalize(inner)),
            TyKind::MutabilityFlexible(inner) => Ty::mutability_flexible(normalize(inner)),
            TyKind::DefinitelyNotNull(inner) => {
                Ty::from(TyKind::DefinitelyNotNull(normalize(inner)))
            }
            TyKind::InProjection(inner) => Ty::in_projection(normalize(inner)),
            TyKind::OutProjection(inner) => Ty::out_projection(normalize(inner)),
            TyKind::StarProjection(inner) => Ty::star_projection(normalize(inner)),
            TyKind::RawProjection(inner) => Ty::raw_projection(normalize(inner)),
            TyKind::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
                name,
                &arguments
                    .iter()
                    .map(|argument| normalize(*argument))
                    .collect::<Vec<_>>(),
            ),
            TyKind::Intersection(parts) => Ty::intersection(
                &parts
                    .iter()
                    .map(|part| normalize(*part))
                    .collect::<Vec<_>>(),
            ),
            _ => ty,
        }
    }

    fn function_component(ty: Ty) -> Ty {
        match ty.kind() {
            TyKind::InProjection(inner)
            | TyKind::OutProjection(inner)
            | TyKind::StarProjection(inner)
            | TyKind::RawProjection(inner) => function_component(inner),
            TyKind::Nullable(inner) => Ty::nullable(function_component(inner)),
            TyKind::PlatformNullable(inner) => Ty::platform_nullable(function_component(inner)),
            TyKind::MutabilityFlexible(inner) => Ty::mutability_flexible(function_component(inner)),
            TyKind::DefinitelyNotNull(inner) => {
                Ty::from(TyKind::DefinitelyNotNull(function_component(inner)))
            }
            _ => ty,
        }
    }

    normalize(ty_subst(ty, bindings))
}

/// Substitute a callable's own `formals`, reading an unbound one as its bound as [`ty_subst`]
/// does. Every other type parameter belongs to a declaration enclosing the call (`T` of the class
/// whose member is called through `this`); it is in scope at the call site, not a variable of the
/// call, so it stays as written.
pub(crate) fn ty_subst_formals<K: TyParamKey>(
    ty: Ty,
    formals: &[String],
    bindings: &HashMap<K, Ty>,
) -> Ty {
    substitute_type_parameters(
        ty,
        |name| recorded_binding(bindings, name),
        |name| !formals.iter().any(|formal| formal == name),
        true,
    )
}

pub(crate) fn ty_subst_all<K: TyParamKey>(types: &[Ty], bindings: &HashMap<K, Ty>) -> Vec<Ty> {
    types.iter().map(|ty| ty_subst(*ty, bindings)).collect()
}

/// Replace the inline bounds carried by type-parameter references throughout a type shape. Signature
/// decoders first discover formal declarations and type uses independently; this single type-model
/// operation joins them without each decoder growing its own recursive walk.
pub(crate) fn ty_with_param_bounds<K: TyParamKey>(ty: Ty, bounds: &HashMap<K, Ty>) -> Ty {
    match ty.kind() {
        TyKind::TyParam(name, current) => Ty::ty_param(
            name.as_str(),
            bounds.get(name.as_str()).copied().unwrap_or(current),
        ),
        TyKind::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| ty_with_param_bounds(*parameter, bounds))
                .collect(),
            ty_with_param_bounds(signature.ret, bounds),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        TyKind::DefinitelyNotNull(inner) => Ty::from(TyKind::DefinitelyNotNull(
            ty_with_param_bounds(inner, bounds),
        )),
        TyKind::Nullable(inner) => Ty::nullable(ty_with_param_bounds(inner, bounds)),
        TyKind::PlatformNullable(inner) => {
            Ty::platform_nullable(ty_with_param_bounds(inner, bounds))
        }
        TyKind::MutabilityFlexible(inner) => {
            Ty::mutability_flexible(ty_with_param_bounds(inner, bounds))
        }
        TyKind::InProjection(inner) => Ty::in_projection(ty_with_param_bounds(inner, bounds)),
        TyKind::OutProjection(inner) => Ty::out_projection(ty_with_param_bounds(inner, bounds)),
        TyKind::StarProjection(inner) => Ty::star_projection(ty_with_param_bounds(inner, bounds)),
        TyKind::RawProjection(inner) => Ty::raw_projection(ty_with_param_bounds(inner, bounds)),
        TyKind::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            name,
            &arguments
                .iter()
                .map(|argument| ty_with_param_bounds(*argument, bounds))
                .collect::<Vec<_>>(),
        ),
        TyKind::Intersection(parts) => Ty::intersection(
            &parts
                .iter()
                .map(|part| ty_with_param_bounds(*part, bounds))
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    }
}

/// Rebuild inline type-parameter bounds from a complete declaration-owned bound map, recursively
/// hydrating references between formals. A declaration such as `T : U, U : A, B` therefore gives
/// occurrences of `T` the full transitive proof. Cycles retain their first repeated occurrence and
/// terminate without inventing a bound.
pub(crate) fn ty_with_transitive_param_bounds<K: TyParamKey>(
    ty: Ty,
    bounds: &HashMap<K, Ty>,
) -> Ty {
    fn hydrate<K: TyParamKey>(
        ty: Ty,
        bounds: &HashMap<K, Ty>,
        visiting: &mut Vec<&'static str>,
    ) -> Ty {
        match ty.kind() {
            TyKind::TyParam(name, current) => {
                if visiting.contains(&name.as_str()) {
                    return ty;
                }
                visiting.push(name.as_str());
                let declared = bounds.get(name.as_str()).copied().unwrap_or(current);
                let hydrated = hydrate(declared, bounds, visiting);
                visiting.pop();
                Ty::ty_param(name.as_str(), hydrated)
            }
            TyKind::Fun(signature) => Ty::fun_with_shape(
                signature
                    .params
                    .iter()
                    .map(|parameter| hydrate(*parameter, bounds, visiting))
                    .collect(),
                hydrate(signature.ret, bounds, visiting),
                signature.context_count,
                signature.has_receiver,
                signature.suspend,
            ),
            TyKind::DefinitelyNotNull(inner) => {
                Ty::from(TyKind::DefinitelyNotNull(hydrate(inner, bounds, visiting)))
            }
            TyKind::Nullable(inner) => Ty::nullable(hydrate(inner, bounds, visiting)),
            TyKind::PlatformNullable(inner) => {
                Ty::platform_nullable(hydrate(inner, bounds, visiting))
            }
            TyKind::MutabilityFlexible(inner) => {
                Ty::mutability_flexible(hydrate(inner, bounds, visiting))
            }
            TyKind::InProjection(inner) => Ty::in_projection(hydrate(inner, bounds, visiting)),
            TyKind::OutProjection(inner) => Ty::out_projection(hydrate(inner, bounds, visiting)),
            TyKind::StarProjection(inner) => Ty::star_projection(hydrate(inner, bounds, visiting)),
            TyKind::RawProjection(inner) => Ty::raw_projection(hydrate(inner, bounds, visiting)),
            TyKind::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
                name,
                &arguments
                    .iter()
                    .map(|argument| hydrate(*argument, bounds, visiting))
                    .collect::<Vec<_>>(),
            ),
            TyKind::Intersection(parts) => Ty::intersection(
                &parts
                    .iter()
                    .map(|part| hydrate(*part, bounds, visiting))
                    .collect::<Vec<_>>(),
            ),
            _ => ty,
        }
    }

    hydrate(ty, bounds, &mut Vec::new())
}

/// Replace declaration-local type-parameter identities throughout a semantic type, including the
/// inline upper bounds carried by nested `TyParam` nodes. Renaming only the outer occurrence leaves
/// chains such as `D : B, B : A` partly keyed by source spelling and breaks bound member lookup.
pub(crate) fn ty_rename_params(ty: Ty, identities: &HashMap<&str, &'static str>) -> Ty {
    match ty.kind() {
        TyKind::TyParam(name, bound) => Ty::ty_param(
            identities
                .get(name.as_str())
                .copied()
                .unwrap_or(name.as_str()),
            ty_rename_params(bound, identities),
        ),
        TyKind::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|parameter| ty_rename_params(*parameter, identities))
                .collect(),
            ty_rename_params(signature.ret, identities),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        TyKind::DefinitelyNotNull(inner) => Ty::from(TyKind::DefinitelyNotNull(ty_rename_params(
            inner, identities,
        ))),
        TyKind::Nullable(inner) => Ty::nullable(ty_rename_params(inner, identities)),
        TyKind::PlatformNullable(inner) => {
            Ty::platform_nullable(ty_rename_params(inner, identities))
        }
        TyKind::MutabilityFlexible(inner) => {
            Ty::mutability_flexible(ty_rename_params(inner, identities))
        }
        TyKind::InProjection(inner) => Ty::in_projection(ty_rename_params(inner, identities)),
        TyKind::OutProjection(inner) => Ty::out_projection(ty_rename_params(inner, identities)),
        TyKind::StarProjection(inner) => Ty::star_projection(ty_rename_params(inner, identities)),
        TyKind::RawProjection(inner) => Ty::raw_projection(ty_rename_params(inner, identities)),
        TyKind::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            name,
            &arguments
                .iter()
                .map(|argument| ty_rename_params(*argument, identities))
                .collect::<Vec<_>>(),
        ),
        TyKind::Intersection(parts) => Ty::intersection(
            &parts
                .iter()
                .map(|part| ty_rename_params(*part, identities))
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    }
}

/// Canonicalize declaration-owned type-parameter identities by their ordinal. This is used for
/// declaration-shape equality: two independently declared `<T>` parameters have different semantic
/// identities during inference, but occupy the same slot when comparing callable signatures.
pub(crate) fn ty_canonicalize_params(ty: Ty, formals: &[String]) -> Ty {
    let identities = formals
        .iter()
        .enumerate()
        .map(|(ordinal, formal)| {
            (
                formal.as_str(),
                intern(&format!("\0signature-parameter:{ordinal}")),
            )
        })
        .collect::<HashMap<_, _>>();
    ty_rename_params(ty, &identities)
}

pub(crate) fn ty_subst_keep_unbound<K: TyParamKey>(ty: Ty, bindings: &HashMap<K, Ty>) -> Ty {
    substitute_type_parameters(ty, |name| recorded_binding(bindings, name), |_| true, true)
}

/// Apply arguments from an already-validated classifier use without narrowing them against the
/// inline bound carried by a symbolic occurrence. Type checking validated `KFunction1<String?, R>`
/// when the classifier was formed; projecting its `invoke` shape must retain `String?` exactly.
pub(crate) fn ty_subst_applied_lookup<F>(ty: Ty, lookup: F) -> Ty
where
    F: Fn(&str) -> Option<Ty> + Copy,
{
    substitute_type_parameters(ty, lookup, |_| true, false)
}

pub(crate) fn ty_subst_applied_arguments<K: TyParamKey>(ty: Ty, bindings: &HashMap<K, Ty>) -> Ty {
    ty_subst_applied_lookup(ty, |name| recorded_binding(bindings, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nullable_bounded_parameter(name: &'static str) -> Ty {
        Ty::ty_param(name, Ty::nullable(Ty::obj("kotlin/Any")))
    }

    #[test]
    fn identity_only_transforms_preserve_definitely_non_null_provenance() {
        let original = nullable_bounded_parameter("owner:T");
        let intersection = Ty::from(TyKind::DefinitelyNotNull(original));
        let identities = HashMap::from_iter([("owner:T", "call:T")]);

        let renamed = ty_rename_params(intersection, &identities);
        let renamed_original = nullable_bounded_parameter("call:T");
        assert_eq!(
            renamed,
            Ty::from(TyKind::DefinitelyNotNull(renamed_original))
        );
        assert_eq!(Ty::nullable(renamed), Ty::nullable(renamed_original));

        let rebound = ty_with_param_bounds(
            intersection,
            &HashMap::from_iter([("owner:T".to_string(), Ty::nullable(Ty::obj("demo/Payload")))]),
        );
        let rebound_original = Ty::ty_param("owner:T", Ty::nullable(Ty::obj("demo/Payload")));
        assert_eq!(
            rebound,
            Ty::from(TyKind::DefinitelyNotNull(rebound_original))
        );
        assert_eq!(Ty::nullable(rebound), Ty::nullable(rebound_original));
    }

    #[test]
    fn transitive_bound_hydration_follows_formals_and_terminates_cycles() {
        let implicit = Ty::nullable(Ty::obj("kotlin/Any"));
        let u = Ty::ty_param("U", implicit);
        let complete_u = Ty::intersection(&[Ty::obj("demo/A"), Ty::obj("demo/B")]);
        let bounds = HashMap::from_iter([("T".to_string(), u), ("U".to_string(), complete_u)]);
        assert_eq!(
            ty_with_transitive_param_bounds(Ty::ty_param("T", implicit), &bounds),
            Ty::ty_param("T", Ty::ty_param("U", complete_u))
        );

        let cyclic = HashMap::from_iter([
            ("T".to_string(), Ty::ty_param("U", implicit)),
            ("U".to_string(), Ty::ty_param("T", implicit)),
        ]);
        assert_eq!(
            ty_with_transitive_param_bounds(Ty::ty_param("T", implicit), &cyclic),
            Ty::ty_param("T", Ty::ty_param("U", Ty::ty_param("T", implicit)))
        );
    }

    #[test]
    fn substitution_preserves_symbolic_provenance_and_consumes_concrete_bindings() {
        let declared = nullable_bounded_parameter("callee:T");
        let intersection = Ty::from(TyKind::DefinitelyNotNull(declared));

        assert_eq!(
            ty_subst_keep_unbound(intersection, &TyParamBindings::default()),
            intersection
        );
        assert_eq!(
            ty_subst(intersection, &TyParamBindings::default()),
            Ty::obj("kotlin/Any")
        );

        let caller = nullable_bounded_parameter("caller:U");
        let symbolic = ty_subst_keep_unbound(
            intersection,
            &HashMap::from_iter([("callee:T".to_string(), caller)]),
        );
        assert_eq!(symbolic, Ty::from(TyKind::DefinitelyNotNull(caller)));
        assert_eq!(Ty::nullable(symbolic), Ty::nullable(caller));

        let concrete = ty_subst_keep_unbound(
            intersection,
            &HashMap::from_iter([("callee:T".to_string(), Ty::nullable(Ty::String))]),
        );
        assert_eq!(concrete, Ty::String);
    }

    #[test]
    fn alias_substitution_consumes_projections_at_function_boundaries_only() {
        let formal = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let template = Ty::fun(vec![formal, Ty::obj_args("sample/Box", &[formal])], formal);
        let bindings = HashMap::from_iter([(
            "T".to_string(),
            Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any"))),
        )]);

        assert_eq!(
            ty_subst_alias_expansion(template, &bindings),
            Ty::fun(
                vec![
                    Ty::nullable(Ty::obj("kotlin/Any")),
                    Ty::obj_args(
                        "sample/Box",
                        &[Ty::star_projection(Ty::nullable(Ty::obj("kotlin/Any")))],
                    ),
                ],
                Ty::nullable(Ty::obj("kotlin/Any")),
            )
        );
    }

    #[test]
    fn applied_argument_substitution_preserves_nullability_against_a_synthetic_bottom_bound() {
        let placeholder = Ty::ty_param("\0signature-type-argument-0", Ty::Nothing);
        let template = Ty::obj_args("sample/Box", &[placeholder]);
        let nullable = Ty::nullable(Ty::obj("kotlin/Any"));
        let bindings = HashMap::from_iter([("\0signature-type-argument-0".to_string(), nullable)]);

        assert_eq!(
            ty_subst_applied_arguments(template, &bindings),
            Ty::obj_args("sample/Box", &[nullable])
        );
    }
}
