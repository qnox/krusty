//! A member call that no candidate accepts during signature evaluation.
//!
//! kotlinc resolves such a call to its one candidate whose parameters the arguments map onto,
//! even when an argument's type does not fit (`j.c(null)` against Java `c(@NotNull String)` and
//! `c(int, int)`): the argument mismatch is that candidate's diagnostic and the call still has the
//! candidate's result. The body check reports the mismatch. Signature evaluation only needs the
//! result, so it must not record "none of the following candidates is applicable" for a call the
//! body check resolves; that list stays for a call no candidate's shape accepts.

use super::*;

impl ProductionSignatureSemantics<'_> {
    /// The result of a member call that selection rejected, or the recorded failure. The call has
    /// a result only when exactly one candidate at the nearest scope rank maps every argument to a
    /// parameter and its semantic result is fixed by the explicit type arguments alone.
    pub(super) fn rejected_member_call(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        receiver: Ty,
        spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[Ty],
        trailing_lambda: bool,
    ) -> Result<crate::fir::ResolvedMemberCall, crate::fir::DiagnosticId> {
        let candidates = self
            .with_resolver(scope, |resolver| {
                Some(
                    resolver
                        .receiver_callables(receiver, spelling)
                        .functions()
                        .to_vec(),
                )
            })
            .unwrap_or_default();
        let names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_owned))
            .collect::<Vec<_>>();
        let names = names
            .iter()
            .any(Option::is_some)
            .then_some(names.as_slice());
        let nearest_rank = candidates
            .iter()
            .map(|candidate| candidate.scope_rank)
            .min();
        let mut mapped = candidates.iter().filter(|candidate| {
            Some(candidate.scope_rank) == nearest_rank
                && candidate.context_count == 0
                && super::super::call_argument_parameter_indices(
                    arguments.len(),
                    candidate.semantic_params().len(),
                    names,
                    trailing_lambda,
                    &candidate.call_sig,
                )
                .is_some()
        });
        if let (Some(candidate), None) = (mapped.next(), mapped.next()) {
            if let Some(ty) = self.rejected_candidate_result(candidate, type_arguments) {
                return Ok(crate::fir::ResolvedMemberCall {
                    ty: Some(ty),
                    declaration: None,
                });
            }
        }
        Err(self.record_member_call_selection_failure(scope, origin, receiver, spelling))
    }

    /// The candidate's semantic result under the call's explicit type arguments. `None` when the
    /// result still names a type parameter: one only argument or receiver inference could fix,
    /// which a rejected call does not perform here.
    fn rejected_candidate_result(
        &self,
        candidate: &crate::libraries::FunctionInfo,
        type_arguments: &[Ty],
    ) -> Option<crate::fir::ResolvedTy> {
        let signature = candidate.semantic_signature();
        if !type_arguments.is_empty() && type_arguments.len() != signature.formals.len() {
            return None;
        }
        let bindings = signature
            .formals
            .iter()
            .cloned()
            .zip(type_arguments.iter().copied())
            .collect::<crate::symbol_resolver::GSigBinds>();
        let specialized = crate::symbol_resolver::ty_subst_keep_unbound(signature.ret, &bindings);
        let result = signature.apply_return_policy(&*self.table.libraries, specialized);
        if result.mentions_ty_param() {
            return None;
        }
        crate::fir::ResolvedTy::new(result).ok()
    }

    fn record_inapplicable_member_call(
        &self,
        declaration: crate::fir::DeclarationId,
        origin: crate::fir::OriginId,
        spelling: &str,
        candidates: &[crate::libraries::FunctionInfo],
    ) -> crate::fir::DiagnosticId {
        let mut displays = candidates
            .iter()
            .map(|candidate| {
                let parameters = candidate
                    .semantic_params()
                    .iter()
                    .enumerate()
                    .map(|(ordinal, parameter)| {
                        let name = candidate
                            .call_sig
                            .param_names
                            .get(ordinal)
                            .filter(|name| !name.is_empty())
                            .cloned()
                            .unwrap_or_else(|| format!("p{ordinal}"));
                        format!("{name}: {}", parameter.source_name())
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "fun {spelling}({parameters}): {}",
                    candidate.callable.ret.source_name()
                )
            })
            .collect::<Vec<_>>();
        displays.sort_unstable();
        displays.dedup();
        displays.truncate(64);
        let mut message = String::from("none of the following candidates is applicable:");
        for (ordinal, display) in displays.into_iter().enumerate() {
            message.push_str(if ordinal == 0 { "\n\n" } else { "\n" });
            message.push_str(&display);
        }
        self.record_source_diagnostic(declaration, origin, message)
    }

    fn record_member_call_selection_failure(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        receiver: Ty,
        spelling: &str,
    ) -> crate::fir::DiagnosticId {
        let candidates = match self.with_resolver(scope, |resolver| {
            Some(
                resolver
                    .receiver_callables(receiver, spelling)
                    .functions()
                    .to_vec(),
            )
        }) {
            Ok(candidates) => candidates,
            Err(diagnostic) => return diagnostic,
        };
        if candidates.is_empty() {
            self.record_unresolved_member(scope, origin, receiver, spelling)
        } else {
            self.record_inapplicable_member_call(scope.owner, origin, spelling, &candidates)
        }
    }
}
