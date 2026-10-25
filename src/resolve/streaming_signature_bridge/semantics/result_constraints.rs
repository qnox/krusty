//! Conversion of compact signature result constraints into the resolver-owned representation.

pub(super) fn signature_call_result_constraint(
    constraint: Option<crate::fir::SignatureCallResultConstraint>,
) -> crate::resolve::CallResultConstraint {
    match constraint {
        None => crate::resolve::CallResultConstraint::None,
        Some(crate::fir::SignatureCallResultConstraint::Direct(expected)) => {
            crate::resolve::CallResultConstraint::Direct(expected.get())
        }
        Some(crate::fir::SignatureCallResultConstraint::SafeLifted(expected)) => {
            crate::resolve::CallResultConstraint::SafeLifted(expected.get())
        }
    }
}

use super::*;

impl ProductionSignatureSemantics<'_> {
    /// A selected member's result restated under the call's expectation. `any<T>()` used where
    /// `Instant` is expected binds `T := Instant` from the result alone, as the top-level and
    /// source-callable paths already do; a result that mentions no open formal is returned as is.
    pub(super) fn member_result_under_expectation(
        &self,
        scope: crate::fir::SignatureScope,
        member: &crate::libraries::LibraryMember,
        result: Ty,
        expected: Option<Ty>,
    ) -> Ty {
        let (Some(expected), Some(generic)) = (expected, member.generic_sig.as_ref()) else {
            return result;
        };
        if generic.formals.is_empty() || !result.mentions_ty_param() {
            return result;
        }
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let semantic_source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&semantic_source);
        let admits = |actual: Ty, bound: Ty| {
            crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &oracle,
                actual,
                bound,
            )
        };
        match crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
            &semantic_source,
            generic,
            expected,
            admits,
        ) {
            Some(bindings) => crate::symbol_resolver::ty_subst_keep_unbound(result, &bindings),
            None => result,
        }
    }
}
