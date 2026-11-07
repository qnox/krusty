//! Receiver-call constraints shared by extension selection and rejected-call diagnostics.

use super::*;

/// Facts fixed by a receiver call independently of its value arguments. Rejected generic
/// candidates also use these constraints when rendering their specialized parameter types.
#[derive(Clone, Copy)]
pub(super) struct CallConstraints<'a> {
    pub(super) type_args: &'a [Ty],
    pub(super) receiver: Ty,
    pub(super) expected: Option<Ty>,
}

impl CallConstraints<'_> {
    /// Receiver evidence is independent of contextual result constraints. Only this evidence can
    /// establish that a projection on the receiver closes a source input.
    pub(super) fn receiver_bindings(
        self,
        source: &dyn crate::symbol_source::SymbolSource,
        signature: &crate::libraries::GenericSig,
    ) -> crate::symbol_resolver::GSigBinds {
        Self {
            expected: None,
            ..self
        }
        .bindings(source, signature)
    }

    pub(super) fn bindings(
        self,
        source: &dyn crate::symbol_source::SymbolSource,
        signature: &crate::libraries::GenericSig,
    ) -> crate::symbol_resolver::GSigBinds {
        let mut bindings = crate::symbol_resolver::seeded_gsig_binds(signature, self.type_args);
        if !self.type_args.is_empty() {
            return bindings;
        }
        let mut seeded = crate::symbol_resolver::GSigBinds::default();
        if let Some(declared) = signature.receiver {
            // An extension receiver is matched through the applied supertype graph. `List<*>`
            // therefore constrains the `T` in `Iterable<T>` to its readable projection, exactly as
            // ordinary overload selection does; structural `unify_ty` cannot see that edge.
            crate::symbol_resolver::unify_ty_from_symbols(
                source,
                declared,
                self.receiver,
                &mut seeded,
            );
        }
        if let Some(expected) = self.expected {
            crate::symbol_resolver::unify_ty(signature.ret, expected, &mut seeded);
        }
        for (formal, ty) in seeded {
            if signature.formals.contains(&formal) && !ty.mentions_pending() && ty != Ty::Error {
                bindings.entry(formal).or_insert(ty);
            }
        }
        bindings
    }
}

/// Whether adding one argument constraint to the active postponed-call frames can still satisfy
/// the type parameters' declared and already-collected upper bounds.
///
/// A symbolic parameter is not automatically applicable. In `Builder<T : Base>.set(T)`, after
/// one call contributes `Value <: T`, a later `set(Other())` with an `Other` outside `Base`
/// joins the lower bounds to their common supertype `Any`. That violates `T : Base`, so the
/// member is inapplicable and the scope tower must be
/// allowed to consider an extension overload. Deferring this check until the outer call is
/// solved loses the overload decision and reports the violation at the wrong call.
pub(super) fn postponed_argument_constraint_is_satisfiable(
    checker: &Checker<'_>,
    expected: Ty,
    actual: Ty,
) -> bool {
    fn collect_declared_bounds(
        ty: Ty,
        formals: &[crate::types::TyParamName],
        bounds: &mut crate::types::TyParamBindings,
    ) {
        match ty.kind() {
            TyKind::TyParam(formal, bound) | TyKind::Var(formal, bound)
                if formals.contains(&formal) =>
            {
                bounds
                    .entry(crate::types::TyParamName::new(&formal))
                    .or_insert(bound);
            }
            TyKind::Obj(_, arguments) => {
                for &argument in arguments {
                    collect_declared_bounds(argument, formals, bounds);
                }
            }
            TyKind::Fun(signature) => {
                for &parameter in &signature.params {
                    collect_declared_bounds(parameter, formals, bounds);
                }
                collect_declared_bounds(signature.ret, formals, bounds);
            }
            TyKind::Nullable(inner)
            | TyKind::PlatformNullable(inner)
            | TyKind::MutabilityFlexible(inner)
            | TyKind::InProjection(inner)
            | TyKind::OutProjection(inner)
            | TyKind::StarProjection(inner) => {
                collect_declared_bounds(inner, formals, bounds);
            }
            _ => {}
        }
    }

    let inferred = crate::symbol_resolver::collect_assignability_constraints_from_symbols(
        &checker.fed_source(),
        expected,
        actual,
    );
    let mut shadowed_formals = crate::fx_hash::HashSet::default();
    for constraints in checker.postponed_call_constraints.iter().rev() {
        let mut declared_bounds = HashMap::default();
        collect_declared_bounds(expected, &constraints.formals, &mut declared_bounds);
        collect_declared_bounds(actual, &constraints.formals, &mut declared_bounds);

        let mut trial = constraints.clone();
        trial.constrain_assignable(
            &checker.fed_source(),
            expected,
            actual,
            &inferred,
            &shadowed_formals,
        );
        for (formal, &lower) in &trial.lower {
            if shadowed_formals.contains(formal.as_str())
                || lower == Ty::Error
                // Evidence may itself be a lexically fixed outer type parameter (`T? <: FT`).
                // It is not a concrete bound violation and must remain available to PCLA.
                || lower.mentions_ty_param()
            {
                continue;
            }
            let bounds = declared_bounds.get(formal).copied().into_iter().chain(
                trial
                    .upper
                    .get(formal.as_str())
                    .into_iter()
                    .flat_map(|bounds| bounds.iter().copied()),
            );
            for bound in bounds {
                let bound = crate::symbol_resolver::ty_subst_keep_unbound(bound, &trial.lower);
                if bound == Ty::Error || bound.mentions_pending() || bound.mentions_ty_param() {
                    continue;
                }
                if !checker.receiver_is_assignable(lower, bound) {
                    return false;
                }
            }
        }
        shadowed_formals.extend(constraints.formals.iter().cloned());
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Source;

    impl crate::symbol_source::SymbolSource for Source {
        fn classifier(&self, _: TypeName) -> Option<std::sync::Arc<crate::libraries::LibraryType>> {
            None
        }
    }

    #[test]
    fn expected_result_projections_do_not_become_receiver_capture_evidence() {
        let parameter = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let receiver = Ty::obj("example/Bucket");
        let signature = crate::libraries::GenericSig {
            formals: vec![crate::types::TyParamName::new("T")],
            formal_bounds: vec![vec![Ty::nullable(Ty::obj("kotlin/Any"))]],
            receiver: Some(receiver),
            params: vec![parameter],
            ret: Ty::obj_args("example/Result", &[parameter]),
            return_policy: crate::libraries::GenericReturnPolicy::Exact,
        };
        let constraints = CallConstraints {
            type_args: &[],
            receiver,
            expected: Some(Ty::obj_args(
                "example/Result",
                &[Ty::out_projection(Ty::Int)],
            )),
        };
        assert_eq!(
            constraints.bindings(&Source, &signature).get("T"),
            Some(&Ty::out_projection(Ty::Int))
        );
        assert!(constraints
            .receiver_bindings(&Source, &signature)
            .is_empty());
    }
}
