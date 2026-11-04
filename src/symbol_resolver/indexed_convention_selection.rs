//! Selection of the indexed-access conventions: the operands written in subscript syntax
//! (`receiver[i]`, `receiver[i] = value`) map onto a `get` or `set` declaration's parameters,
//! including a vararg index that ordinary positional calls cannot bind before a final value.

use super::overload_selection::{
    select_receiver_overload_from_functions_tracking, IndexedConvention, OverloadCall,
};
use super::*;

impl SymbolResolver<'_> {
    pub(crate) fn select_receiver_indexed_get_function_with_params(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
    ) -> CandidateSelection<(FunctionInfo, Vec<Ty>, Ty, GSigBinds)> {
        let selected = match select_receiver_overload_from_functions_tracking(
            self.lib,
            OverloadCall {
                recv: receiver,
                name,
                args,
                type_args,
                indexed: IndexedConvention::Get,
            },
            self.ext_ctx(),
            callables.functions(),
        ) {
            CandidateSelectionWithTies::Selected(selected) => selected,
            CandidateSelectionWithTies::Ambiguous(_) => return CandidateSelection::Ambiguous,
            CandidateSelectionWithTies::None => return CandidateSelection::None,
        };
        let binding_receiver = selected
            .semantic_receiver()
            .and_then(|declared| {
                self.receiver_mro(receiver)
                    .binding_receiver(&self.src, declared)
            })
            .unwrap_or(receiver);
        if selected.call_sig.vararg_index.is_none() {
            let params =
                logical_call_params(&self.src, &selected, binding_receiver, args, type_args);
            // The bindings are published with the call; the receiver binds `V` of `Map.get`.
            let (resolved, bindings) = if selected.generic_sig.is_some() && selected.is_extension()
            {
                let arg_tys = args.iter().map(CallArgKind::ty).collect::<Vec<_>>();
                let semantic = selected.semantic_signature();
                let (ret, bindings) = bind_ext_ret_tracking(
                    &self.src,
                    &semantic,
                    binding_receiver,
                    &arg_tys,
                    type_args,
                );
                (
                    specialized_extension_return(self.lib, &selected, ret),
                    bindings,
                )
            } else if selected.is_extension() {
                (
                    selected.ret.apply(selected.callable.ret),
                    GSigBinds::default(),
                )
            } else {
                let (member, bindings) = resolved_member_from_info_tracking(
                    self.lib,
                    &self.src,
                    receiver,
                    args,
                    type_args,
                    selected.clone(),
                );
                (member.ret, bindings)
            };
            return CandidateSelection::Selected((selected, params, resolved, bindings));
        }
        let Some((params, ret, bindings)) = indexed_call_shape(
            self.lib,
            &self.src,
            &selected,
            binding_receiver,
            args,
            type_args,
            false,
        ) else {
            return CandidateSelection::None;
        };
        CandidateSelection::Selected((selected, params, ret, bindings))
    }

    /// Select the `set` convention used by indexed assignment. The assignment RHS binds the final
    /// value parameter even when the preceding index parameter is a vararg; ordinary positional
    /// calls deliberately do not permit that mapping.
    pub(crate) fn select_receiver_indexed_set_function(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
    ) -> CandidateSelection<FunctionInfo> {
        select_receiver_overload_from_functions_tracking(
            self.lib,
            OverloadCall {
                recv: receiver,
                name,
                args,
                type_args,
                indexed: IndexedConvention::Set,
            },
            self.ext_ctx(),
            callables.functions(),
        )
        .collapse()
    }

    pub(crate) fn select_receiver_indexed_set_function_with_params(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
    ) -> CandidateSelection<(FunctionInfo, Vec<Ty>, Ty)> {
        let selected = match self
            .select_receiver_indexed_set_function(receiver, name, args, type_args, callables)
        {
            CandidateSelection::Selected(selected) => selected,
            CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
            CandidateSelection::None => return CandidateSelection::None,
        };
        let binding_receiver = selected
            .semantic_receiver()
            .and_then(|declared| {
                self.receiver_mro(receiver)
                    .binding_receiver(&self.src, declared)
            })
            .unwrap_or(receiver);
        if selected.call_sig.vararg_index.is_none() {
            let params =
                logical_call_params(&self.src, &selected, binding_receiver, args, type_args);
            let arg_tys = args.iter().map(CallArgKind::ty).collect::<Vec<_>>();
            let inferred_ret = selected
                .generic_sig
                .as_ref()
                .map_or(selected.callable.ret, |sig| {
                    if selected.is_extension() {
                        bind_ext_ret(&self.src, sig, binding_receiver, &arg_tys, type_args)
                    } else {
                        bind_member_return(
                            &self.src,
                            sig,
                            binding_receiver,
                            &arg_tys,
                            type_args,
                            selected.callable.ret,
                        )
                    }
                });
            let ret = selected.ret.apply(inferred_ret);
            return CandidateSelection::Selected((selected, params, ret));
        }
        let Some((params, ret, _)) = indexed_call_shape(
            self.lib,
            &self.src,
            &selected,
            binding_receiver,
            args,
            type_args,
            true,
        ) else {
            return CandidateSelection::None;
        };
        CandidateSelection::Selected((selected, params, ret))
    }
}
