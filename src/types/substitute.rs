//! Type-parameter substitution. Callers supply a lookup; the walk never builds a map of its own.

use std::collections::HashMap;

use super::{intern, Ty};

fn compose_in_projection(inner: Ty) -> Ty {
    match inner {
        Ty::StarProjection(_) | Ty::InProjection(_) => inner,
        other => Ty::in_projection(other),
    }
}

fn compose_out_projection(inner: Ty) -> Ty {
    match inner {
        Ty::StarProjection(_) | Ty::OutProjection(_) => inner,
        other => Ty::out_projection(other),
    }
}

/// Substitute semantic type parameters throughout one type shape. This belongs to the type model:
/// providers, overload selection, checking, and lowering all consume the same transformation.
fn substitute_type_parameters<F>(
    ty: Ty,
    lookup: F,
    preserve_unbound: bool,
    enforce_bound_nullability: bool,
) -> Ty
where
    F: Fn(&str) -> Option<Ty> + Copy,
{
    match ty {
        Ty::TyParam(name, bound) => lookup(name)
            .map(|binding| {
                if !enforce_bound_nullability || bound.upper_bound_admits_null() {
                    binding
                } else {
                    binding.non_null()
                }
            })
            .unwrap_or_else(|| {
                if preserve_unbound {
                    ty
                } else {
                    bound.non_null()
                }
            }),
        Ty::Fun(signature) => Ty::fun_with_shape(
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
        Ty::DefinitelyNotNull(inner) => match *inner {
            Ty::TyParam(name, bound) => match lookup(name) {
                Some(binding) => binding.contributed_through_nullable_formal(),
                None if preserve_unbound => Ty::DefinitelyNotNull(inner),
                None => bound.contributed_through_nullable_formal(),
            },
            other => substitute_type_parameters(
                other,
                lookup,
                preserve_unbound,
                enforce_bound_nullability,
            )
            .contributed_through_nullable_formal(),
        },
        Ty::Nullable(inner) => Ty::nullable(substitute_type_parameters(
            *inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(substitute_type_parameters(
            *inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        Ty::InProjection(inner) => compose_in_projection(substitute_type_parameters(
            *inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        Ty::OutProjection(inner) => compose_out_projection(substitute_type_parameters(
            *inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        Ty::StarProjection(inner) => Ty::star_projection(substitute_type_parameters(
            *inner,
            lookup,
            preserve_unbound,
            enforce_bound_nullability,
        )),
        Ty::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
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
        Ty::Intersection(parts) => Ty::intersection(
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

fn recorded_binding(bindings: &HashMap<String, Ty>, name: &str) -> Option<Ty> {
    bindings.get(name).copied()
}

pub(crate) fn ty_subst(ty: Ty, bindings: &HashMap<String, Ty>) -> Ty {
    substitute_type_parameters(ty, |name| recorded_binding(bindings, name), false, true)
}

pub(crate) fn ty_subst_all(types: &[Ty], bindings: &HashMap<String, Ty>) -> Vec<Ty> {
    types.iter().map(|ty| ty_subst(*ty, bindings)).collect()
}

/// Replace the inline bounds carried by type-parameter references throughout a type shape. Signature
/// decoders first discover formal declarations and type uses independently; this single type-model
/// operation joins them without each decoder growing its own recursive walk.
pub(crate) fn ty_with_param_bounds(ty: Ty, bounds: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::TyParam(name, current) => {
            Ty::ty_param(name, bounds.get(name).copied().unwrap_or(*current))
        }
        Ty::Fun(signature) => Ty::fun_with_shape(
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
        Ty::DefinitelyNotNull(inner) => {
            Ty::DefinitelyNotNull(super::intern_ty(ty_with_param_bounds(*inner, bounds)))
        }
        Ty::Nullable(inner) => Ty::nullable(ty_with_param_bounds(*inner, bounds)),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(ty_with_param_bounds(*inner, bounds)),
        Ty::InProjection(inner) => Ty::in_projection(ty_with_param_bounds(*inner, bounds)),
        Ty::OutProjection(inner) => Ty::out_projection(ty_with_param_bounds(*inner, bounds)),
        Ty::StarProjection(inner) => Ty::star_projection(ty_with_param_bounds(*inner, bounds)),
        Ty::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            name,
            &arguments
                .iter()
                .map(|argument| ty_with_param_bounds(*argument, bounds))
                .collect::<Vec<_>>(),
        ),
        Ty::Intersection(parts) => Ty::intersection(
            &parts
                .iter()
                .map(|part| ty_with_param_bounds(*part, bounds))
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    }
}

/// Replace declaration-local type-parameter identities throughout a semantic type, including the
/// inline upper bounds carried by nested `TyParam` nodes. Renaming only the outer occurrence leaves
/// chains such as `D : B, B : A` partly keyed by source spelling and breaks bound member lookup.
pub(crate) fn ty_rename_params(ty: Ty, identities: &HashMap<&str, &'static str>) -> Ty {
    match ty {
        Ty::TyParam(name, bound) => Ty::ty_param(
            identities.get(name).copied().unwrap_or(name),
            ty_rename_params(*bound, identities),
        ),
        Ty::Fun(signature) => Ty::fun_with_shape(
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
        Ty::DefinitelyNotNull(inner) => {
            Ty::DefinitelyNotNull(super::intern_ty(ty_rename_params(*inner, identities)))
        }
        Ty::Nullable(inner) => Ty::nullable(ty_rename_params(*inner, identities)),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(ty_rename_params(*inner, identities)),
        Ty::InProjection(inner) => Ty::in_projection(ty_rename_params(*inner, identities)),
        Ty::OutProjection(inner) => Ty::out_projection(ty_rename_params(*inner, identities)),
        Ty::StarProjection(inner) => Ty::star_projection(ty_rename_params(*inner, identities)),
        Ty::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            name,
            &arguments
                .iter()
                .map(|argument| ty_rename_params(*argument, identities))
                .collect::<Vec<_>>(),
        ),
        Ty::Intersection(parts) => Ty::intersection(
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

pub(crate) fn ty_subst_keep_unbound(ty: Ty, bindings: &HashMap<String, Ty>) -> Ty {
    substitute_type_parameters(ty, |name| recorded_binding(bindings, name), true, true)
}

/// Apply arguments from an already-validated classifier use without narrowing them against the
/// inline bound carried by a symbolic occurrence. Type checking validated `KFunction1<String?, R>`
/// when the classifier was formed; projecting its `invoke` shape must retain `String?` exactly.
pub(crate) fn ty_subst_applied_lookup<F>(ty: Ty, lookup: F) -> Ty
where
    F: Fn(&str) -> Option<Ty> + Copy,
{
    substitute_type_parameters(ty, lookup, true, false)
}

pub(crate) fn ty_subst_applied_arguments(ty: Ty, bindings: &HashMap<String, Ty>) -> Ty {
    ty_subst_applied_lookup(ty, |name| recorded_binding(bindings, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::intern_ty;

    fn nullable_bounded_parameter(name: &'static str) -> Ty {
        Ty::ty_param(name, Ty::nullable(Ty::obj("kotlin/Any")))
    }

    #[test]
    fn identity_only_transforms_preserve_definitely_non_null_provenance() {
        let original = nullable_bounded_parameter("owner:T");
        let intersection = Ty::DefinitelyNotNull(intern_ty(original));
        let identities = HashMap::from([("owner:T", "call:T")]);

        let renamed = ty_rename_params(intersection, &identities);
        let renamed_original = nullable_bounded_parameter("call:T");
        assert_eq!(renamed, Ty::DefinitelyNotNull(intern_ty(renamed_original)));
        assert_eq!(Ty::nullable(renamed), Ty::nullable(renamed_original));

        let rebound = ty_with_param_bounds(
            intersection,
            &HashMap::from([("owner:T".to_string(), Ty::nullable(Ty::obj("demo/Payload")))]),
        );
        let rebound_original = Ty::ty_param("owner:T", Ty::nullable(Ty::obj("demo/Payload")));
        assert_eq!(rebound, Ty::DefinitelyNotNull(intern_ty(rebound_original)));
        assert_eq!(Ty::nullable(rebound), Ty::nullable(rebound_original));
    }

    #[test]
    fn substitution_preserves_symbolic_provenance_and_consumes_concrete_bindings() {
        let declared = nullable_bounded_parameter("callee:T");
        let intersection = Ty::DefinitelyNotNull(intern_ty(declared));

        assert_eq!(
            ty_subst_keep_unbound(intersection, &HashMap::new()),
            intersection
        );
        assert_eq!(
            ty_subst(intersection, &HashMap::new()),
            Ty::obj("kotlin/Any")
        );

        let caller = nullable_bounded_parameter("caller:U");
        let symbolic = ty_subst_keep_unbound(
            intersection,
            &HashMap::from([("callee:T".to_string(), caller)]),
        );
        assert_eq!(symbolic, Ty::DefinitelyNotNull(intern_ty(caller)));
        assert_eq!(Ty::nullable(symbolic), Ty::nullable(caller));

        let concrete = ty_subst_keep_unbound(
            intersection,
            &HashMap::from([("callee:T".to_string(), Ty::nullable(Ty::String))]),
        );
        assert_eq!(concrete, Ty::String);
    }
}
