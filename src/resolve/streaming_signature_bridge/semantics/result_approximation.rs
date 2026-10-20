//! Projection of solver-local types into denotable declaration results.

use crate::types::{Ty, TyKind};

/// Remove solver-local projection captures from an inferred declaration result. A capture is
/// readable through its upper bound at the result root; inside a generic argument it is exposed as
/// a star projection so the published signature does not claim an invariant type callers cannot
/// name.
pub(super) fn denotable_signature_result(
    ty: Ty,
    denotable_parameters: &crate::fx_hash::HashSet<String>,
) -> Ty {
    fn result(
        ty: Ty,
        denotable: &crate::fx_hash::HashSet<String>,
        visiting: &mut crate::fx_hash::HashSet<&'static str>,
    ) -> Ty {
        match ty.kind() {
            // `Null` is the solver's literal-only bottom marker. A declaration inferred from that
            // expression publishes Kotlin's denotable `Nothing?`; the marker must not escape.
            TyKind::Null => Ty::nullable(Ty::Nothing),
            TyKind::TyParam(name, bound) if !denotable.contains(name.as_str()) => {
                if !visiting.insert(name.as_str()) {
                    return Ty::nullable(Ty::obj("kotlin/Any"));
                }
                let approximated = result(bound.projection_read_ty(), denotable, visiting);
                visiting.remove(name.as_str());
                approximated
            }
            TyKind::TyParam(..) => ty,
            TyKind::Obj(owner, arguments) if !arguments.is_empty() => Ty::obj_args_name(
                owner,
                &arguments
                    .iter()
                    .map(|argument| {
                        let argument = *argument;
                        match argument.kind() {
                            TyKind::TyParam(name, bound) if !denotable.contains(name.as_str()) => {
                                Ty::star_projection(result(
                                    bound.projection_read_ty(),
                                    denotable,
                                    visiting,
                                ))
                            }
                            TyKind::InProjection(inner) => {
                                Ty::in_projection(result(inner, denotable, visiting))
                            }
                            TyKind::OutProjection(inner) => {
                                Ty::out_projection(result(inner, denotable, visiting))
                            }
                            TyKind::StarProjection(inner) => {
                                Ty::star_projection(result(inner, denotable, visiting))
                            }
                            _ => result(argument, denotable, visiting),
                        }
                    })
                    .collect::<Vec<_>>(),
            ),
            TyKind::Fun(signature) => Ty::fun_with_shape(
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
            TyKind::Nullable(inner) => Ty::nullable(result(inner, denotable, visiting)),
            TyKind::PlatformNullable(inner) => {
                Ty::platform_nullable(result(inner, denotable, visiting))
            }
            TyKind::InProjection(inner) => Ty::in_projection(result(inner, denotable, visiting)),
            TyKind::OutProjection(inner) => Ty::out_projection(result(inner, denotable, visiting)),
            TyKind::StarProjection(inner) => {
                Ty::star_projection(result(inner, denotable, visiting))
            }
            _ => ty,
        }
    }

    result(
        ty,
        denotable_parameters,
        &mut crate::fx_hash::HashSet::default(),
    )
}
