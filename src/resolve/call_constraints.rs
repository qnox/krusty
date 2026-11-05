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
