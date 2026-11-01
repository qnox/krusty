//! Generic bindings of a selected member call, applied to its parameters and its result.

use super::*;

impl Checker<'_> {
    /// The selected member's parameters with its formals bound at this call.
    pub(super) fn applied_member_call_params(
        &self,
        receiver: Ty,
        member: &crate::libraries::LibraryMember,
        call: MemberCallArguments<'_>,
    ) -> Result<Option<Vec<Ty>>, CallArgMappingFailure> {
        let Some(signature) = member.generic_sig.as_ref() else {
            return Ok(Some(member.params.clone()));
        };
        Ok(self
            .member_call_bindings(receiver, member, signature, call)?
            .map(|bindings| applied_types(&signature.params, &bindings)))
    }

    /// Specialize a member selected by `super.name(...)` to this call. A super call dispatches to
    /// the declaration itself, but its formals are still inferred from the arguments like any
    /// member call's: `super.wrap(expr)` binds `fun <T> wrap(e: Expr<T>): Expr<T>` to the
    /// caller's own `T`.
    pub(super) fn specialize_super_member(
        &mut self,
        scope: &CheckerScope<'_>,
        call: ExprId,
        receiver: Ty,
        arguments: &[CallArgKind],
        member: &mut crate::libraries::LibraryMember,
    ) {
        let Some(signature) = member.generic_sig.clone() else {
            return;
        };
        let type_arguments = self.resolved_explicit_type_args(scope, call);
        let names = self.file.call_arg_names.get(&call.0).map(Vec::as_slice);
        let call_arguments = MemberCallArguments {
            values: arguments,
            names,
            trailing_lambda: self.file.call_has_trailing_lambda.contains(&call.0),
            type_arguments: &type_arguments,
            fixed_bindings: None,
        };
        if let Ok(Some(bindings)) =
            self.member_call_bindings(receiver, member, &signature, call_arguments)
        {
            member.params = applied_types(&signature.params, &bindings);
            member.ret = crate::symbol_resolver::ty_subst_keep_unbound(member.ret, &bindings);
        }
    }

    /// Bind the selected member's formals at this call: the receiver's type arguments, explicit
    /// type arguments, and what the arguments infer. `None` when a binding violates its bound.
    fn member_call_bindings(
        &self,
        receiver: Ty,
        member: &crate::libraries::LibraryMember,
        signature: &crate::libraries::GenericSig,
        call: MemberCallArguments<'_>,
    ) -> Result<Option<crate::symbol_resolver::GSigBinds>, CallArgMappingFailure> {
        let mut bindings = self.member_receiver_type_bindings(receiver, member);
        for (formal, actual) in
            crate::symbol_resolver::seeded_gsig_binds(signature, call.type_arguments)
        {
            bindings.insert(formal, actual);
        }
        if let Some(fixed) = call.fixed_bindings {
            bindings.extend(fixed.iter().map(|(formal, actual)| (*formal, *actual)));
        }
        let context_count = member.context_count.min(signature.params.len());
        let explicitly_named = |parameter: usize| {
            self.file.explicit_context_arguments
                && member
                    .call_sig
                    .param_names
                    .get(parameter)
                    .is_some_and(|parameter| {
                        call.names.is_some_and(|names| {
                            names.iter().flatten().any(|argument| argument == parameter)
                        })
                    })
        };
        let visible_parameters = (context_count..signature.params.len())
            .chain((0..context_count).filter(|&parameter| explicitly_named(parameter)))
            .collect::<Vec<_>>();
        let visible_call_sig = member.call_sig.select_parameters(&visible_parameters);
        let parameter_indices = call_argument_parameter_indices_result(
            call.values.len(),
            visible_parameters.len(),
            call.names,
            call.trailing_lambda,
            &visible_call_sig,
        )?
        .into_iter()
        .map(|parameter| visible_parameters[parameter])
        .collect::<Vec<_>>();
        let mut inferred = crate::symbol_resolver::infer_generic_call_bindings_from_symbols(
            &self.fed_source(),
            signature,
            parameter_indices
                .iter()
                .copied()
                .zip(call.values)
                .enumerate()
                .filter_map(|(source, (parameter, argument))| {
                    let declared = *signature.params.get(parameter)?;
                    let named_whole_array = member.call_sig.vararg_index == Some(parameter)
                        && call
                            .names
                            .and_then(|names| names.get(source))
                            .is_some_and(Option::is_some);
                    (!argument.is_expected_type_callable()).then(|| {
                        (
                            parameter,
                            argument.type_for(declared),
                            argument.is_spread() || named_whole_array,
                        )
                    })
                }),
            member.call_sig.vararg_index,
        );
        if let Some(fixed) = call.fixed_bindings {
            inferred.retain(|formal, _| !fixed.contains_key(formal));
        }
        crate::trace_compiler!(
            "expected_call",
            "member generic arguments formals={:?} signature_params={:?} arguments={:?} receiver_bindings={bindings:?} inferred={inferred:?}",
            signature.formals,
            signature.params,
            call.values,
        );
        crate::symbol_resolver::merge_generic_bindings(
            signature,
            call.type_arguments.len(),
            &mut bindings,
            inferred,
        );
        Ok(crate::symbol_resolver::generic_bindings_satisfy_bounds(
            signature,
            &bindings,
            |actual, bound| self.generic_bound_admits(actual, bound),
        )
        .then_some(bindings))
    }
}

fn applied_types(types: &[Ty], bindings: &crate::symbol_resolver::GSigBinds) -> Vec<Ty> {
    types
        .iter()
        .map(|ty| crate::symbol_resolver::ty_subst_keep_unbound(*ty, bindings))
        .collect()
}
