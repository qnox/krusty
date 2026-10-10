//! Projection of solver-local types into denotable declaration results.

use crate::types::Ty;

/// Remove solver-local projection captures from an inferred declaration result. A capture is
/// readable through its upper bound at the result root; inside a generic argument it is exposed as
/// a star projection so the published signature does not claim an invariant type callers cannot
/// name.
pub(super) fn denotable_signature_result(
    ty: Ty,
    denotable_parameters: &std::collections::HashSet<String>,
) -> Ty {
    fn result(
        ty: Ty,
        denotable: &std::collections::HashSet<String>,
        visiting: &mut std::collections::HashSet<&'static str>,
    ) -> Ty {
        match ty {
            // `Null` is the solver's literal-only bottom marker. A declaration inferred from that
            // expression publishes Kotlin's denotable `Nothing?`; the marker must not escape.
            Ty::Null => Ty::nullable(Ty::Nothing),
            Ty::TyParam(name, bound) if !denotable.contains(name) => {
                if !visiting.insert(name) {
                    return Ty::nullable(Ty::obj("kotlin/Any"));
                }
                let approximated = result(bound.projection_read_ty(), denotable, visiting);
                visiting.remove(name);
                approximated
            }
            Ty::TyParam(..) => ty,
            Ty::Obj(owner, arguments) if !arguments.is_empty() => Ty::obj_args_name(
                owner,
                &arguments
                    .iter()
                    .map(|argument| match *argument {
                        Ty::TyParam(name, bound) if !denotable.contains(name) => {
                            Ty::star_projection(result(
                                bound.projection_read_ty(),
                                denotable,
                                visiting,
                            ))
                        }
                        Ty::InProjection(inner) => {
                            Ty::in_projection(result(*inner, denotable, visiting))
                        }
                        Ty::OutProjection(inner) => {
                            Ty::out_projection(result(*inner, denotable, visiting))
                        }
                        Ty::StarProjection(inner) => {
                            Ty::star_projection(result(*inner, denotable, visiting))
                        }
                        argument => result(argument, denotable, visiting),
                    })
                    .collect::<Vec<_>>(),
            ),
            Ty::Fun(signature) => Ty::fun_with_shape(
                signature
                    .params
                    .iter()
                    .map(|parameter| result(*parameter, denotable, visiting))
                    .collect(),
                result(signature.ret, denotable, visiting),
                signature.context_count,
                signature.has_receiver,
                signature.suspend,
            ),
            Ty::Nullable(inner) => Ty::nullable(result(*inner, denotable, visiting)),
            Ty::PlatformNullable(inner) => {
                Ty::platform_nullable(result(*inner, denotable, visiting))
            }
            Ty::InProjection(inner) => Ty::in_projection(result(*inner, denotable, visiting)),
            Ty::OutProjection(inner) => Ty::out_projection(result(*inner, denotable, visiting)),
            Ty::StarProjection(inner) => Ty::star_projection(result(*inner, denotable, visiting)),
            _ => ty,
        }
    }

    result(
        ty,
        denotable_parameters,
        &mut std::collections::HashSet::new(),
    )
}
