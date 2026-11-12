//! Projection of solver-local types into denotable declaration results.

use crate::types::{Ty, TyKind};

mod anonymous_results;

/// Publish a denotable declaration result without changing the checked expression that produced it.
pub(super) fn declaration_result(
    semantics: &super::ProductionSignatureSemantics<'_>,
    declaration: crate::fir::DeclarationId,
    result: crate::fir::ResolvedTy,
    evaluated_body: crate::fir::ResolvedTy,
) -> Result<crate::fir::DeclarationResultTypes, crate::fir::DiagnosticId> {
    // Only a script's `$$result` may be `Nothing`: the script ends by throwing.
    if result.get() == Ty::Nothing
        && semantics.headers.stub(declaration).is_some_and(|stub| {
            stub.kind == crate::fir::DeclarationKind::Property
                && !stub.flags.has(crate::fir::DeclarationFlags::SCRIPT_RESULT)
        })
    {
        return Err(semantics.record_missing_signature(declaration));
    }
    let anchor = semantics
        .headers
        .declarations
        .anchor(declaration)
        .ok_or_else(super::ProductionSignatureSemantics::failure)?;
    let scope = crate::fir::SignatureScope {
        owner: declaration,
        source: anchor.source,
    };
    let denotable = semantics.with_signature_type_scope(scope, |lexical| {
        lexical
            .lexical_tparam_identities()
            .into_iter()
            .collect::<crate::fx_hash::HashSet<_>>()
    })?;
    let exposed = anonymous_results::declaration_result(semantics, declaration, result.get())?;
    let resolve = |ty| {
        crate::fir::ResolvedTy::new(denotable_signature_result(ty, &denotable))
            .map_err(|_| super::ProductionSignatureSemantics::failure())
    };
    // Ordinary solver-local approximation must not retain a callee's temporary formal as checking
    // context. Retain a separate body view only for anonymous exposure, using the same lexical
    // normalization without applying anonymous exposure to that body.
    Ok(crate::fir::DeclarationResultTypes {
        published: resolve(exposed)?,
        inferred_body_result: if exposed != result.get() {
            Some(resolve(evaluated_body.get())?)
        } else {
            None
        },
    })
}

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
            // An inferred `A & B` is not a signature type. Approximate it to the denotable
            // supertype kotlinc publishes (`Any` when the parts share no classifier) before the
            // result is lowered. A same-classifier intersection stays when its parameter variance
            // is unknown: inventing one would disagree with the checked approximation.
            TyKind::Intersection(_) => {
                match crate::types::declaration_approximation(ty, &mut |_, _| None) {
                    Some(approximated) if approximated != ty => {
                        result(approximated, denotable, visiting)
                    }
                    _ => ty,
                }
            }
            TyKind::TyParam(name, bound) | TyKind::Var(name, bound)
                if !denotable.contains(name.as_str()) =>
            {
                if !visiting.insert(name.as_str()) {
                    return Ty::nullable(Ty::obj("kotlin/Any"));
                }
                let approximated = result(bound.projection_read_ty(), denotable, visiting);
                visiting.remove(name.as_str());
                approximated
            }
            TyKind::TyParam(..) | TyKind::Var(..) => ty,
            TyKind::Obj(owner, arguments) if !arguments.is_empty() => Ty::obj_args_name(
                owner,
                &arguments
                    .iter()
                    .map(|argument| {
                        let argument = *argument;
                        match argument.kind() {
                            TyKind::TyParam(name, bound) | TyKind::Var(name, bound)
                                if !denotable.contains(name.as_str()) =>
                            {
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
            TyKind::MutabilityFlexible(inner) => {
                Ty::mutability_flexible(result(inner, denotable, visiting))
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

#[cfg(test)]
mod tests {
    use super::denotable_signature_result;
    use crate::types::Ty;

    fn publish(ty: Ty) -> Ty {
        denotable_signature_result(ty, &crate::fx_hash::HashSet::default())
    }

    #[test]
    fn an_inferred_intersection_publishes_a_denotable_declaration_type() {
        let intersection = Ty::intersection(&[Ty::obj("sample/A"), Ty::obj("sample/B")]);
        assert_eq!(publish(intersection), Ty::obj("kotlin/Any"));
        // `fun foobar(a: A, b: B) = select(a, b)` infers `In<A & B>`. The argument is what
        // metadata would otherwise try to write.
        assert_eq!(
            publish(Ty::obj_args("sample/In", &[intersection])),
            Ty::obj_args("sample/In", &[Ty::obj("kotlin/Any")])
        );
        assert_eq!(
            publish(Ty::nullable(intersection)),
            Ty::nullable(Ty::obj("kotlin/Any"))
        );
        assert_eq!(publish(Ty::Null), Ty::nullable(Ty::Nothing));
        assert_eq!(publish(Ty::Int), Ty::Int);
    }
}
