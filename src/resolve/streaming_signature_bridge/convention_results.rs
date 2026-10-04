//! Applying one selected convention to a demanded source signature.
//!
//! Selection owns the expected-result constraint. This boundary reapplies the same receiver,
//! arguments and result expectation when a source declaration's final signature was demanded
//! after selection, so the selected callable is never re-resolved or instantiated differently.

use super::*;

impl ProductionSignatureSemantics<'_> {
    fn apply_demanded_function(
        &self,
        receiver: Ty,
        selected: &crate::libraries::FunctionInfo,
        signature: &crate::fir::ResolvedSignature,
        arguments: &[Ty],
        expected_result: Option<Ty>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        let mut bindings = crate::symbol_resolver::GSigBinds::new();
        let dispatch_member = selected.kind == crate::libraries::FnKind::Member;
        if !dispatch_member {
            if let Some(declared) = selected.semantic_receiver() {
                crate::symbol_resolver::unify_inferred_ty(declared, receiver, &mut bindings);
            }
        }
        let specialize_dispatch = |ty| {
            if dispatch_member {
                self.apply_dispatch_owner(
                    receiver,
                    selected.callable.owner,
                    selected.generic_sig.as_ref(),
                    ty,
                )
            } else {
                ty
            }
        };
        if let Some(expected) = expected_result {
            crate::symbol_resolver::unify_inferred_ty(
                specialize_dispatch(signature.result.get()),
                expected,
                &mut bindings,
            );
        }
        let context_count = selected.context_count.min(signature.parameters.len());
        for (parameter, argument) in signature.parameters[context_count..].iter().zip(arguments) {
            crate::symbol_resolver::unify_inferred_ty(
                specialize_dispatch(parameter.get()),
                *argument,
                &mut bindings,
            );
        }
        crate::fir::ResolvedTy::new(crate::symbol_resolver::ty_subst_keep_unbound(
            specialize_dispatch(signature.result.get()),
            &bindings,
        ))
        .map_err(|_| Self::failure())
    }

    pub(super) fn selected_convention_result(
        &self,
        receiver: Ty,
        selected: &crate::libraries::FunctionInfo,
        result: Ty,
        arguments: &[Ty],
        expected_result: Option<Ty>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        if let Some(signature) =
            self.demanded_member_signature(selected.stable_declaration, demand)?
        {
            return self.apply_demanded_function(
                receiver,
                selected,
                &signature,
                arguments,
                expected_result,
            );
        }
        if let Some(signature) =
            self.demanded_source_signature(None, selected.stable_declaration, demand)?
        {
            crate::trace_compiler!(
                "signature",
                "demanded convention receiver={receiver:?} parameters={:?} result={:?}",
                signature.parameters,
                signature.result,
            );
            return self.apply_demanded_function(
                receiver,
                selected,
                &signature,
                arguments,
                expected_result,
            );
        }
        crate::fir::ResolvedTy::new(result).map_err(|_| panic!("unresolved convention result"))
    }
}
