//! Declaration-scope iterator lookup retained for collection-transform migration.
//!
//! `map`/`flatMap` collection plans do not yet carry the stable traversal identities published by
//! ordinary iteration plans. Keep their declaration-owned lookup isolated here until those plans
//! migrate, then delete this bridge as a unit.

use super::*;

impl Checker<'_> {
    /// Resolve one operator convention in the declaration-owned scope attached to a selected
    /// collection-transform body. The provider capability supplies the package scope; the checker
    /// performs the ordinary member-then-extension precedence once and records the exact call for
    /// lowering.
    fn declaration_zero_arg_operator(
        &self,
        recv: Ty,
        name: &str,
        declaration_scope: &[TypeName],
    ) -> Option<ResolvedCall> {
        let resolver =
            crate::symbol_resolver::SymbolResolver::new_scoped(self.libraries, declaration_scope);
        let callables = resolver.receiver_callables(recv, name);
        for kind in [
            crate::libraries::FnKind::Member,
            crate::libraries::FnKind::Extension,
        ] {
            let (mut functions, _) = callables.clone().into_parts();
            functions
                .overloads
                .retain(|candidate| candidate.kind == kind);
            let candidates = crate::libraries::Callables::Functions(functions);
            let (selected, params, ret) = match resolver
                .select_receiver_function_with_params_tracking(recv, name, &[], &[], &candidates)
            {
                crate::symbol_resolver::CandidateSelection::Selected(selected) => selected,
                crate::symbol_resolver::CandidateSelection::None => continue,
                crate::symbol_resolver::CandidateSelection::Ambiguous => return None,
            };
            if !params.is_empty() || selected.context_count != 0 || !selected.flags.operator {
                return None;
            }
            if kind == crate::libraries::FnKind::Extension {
                let mut callable = selected.callable;
                callable.ret = ret;
                return Some(ResolvedCall::library_extension(callable));
            }
            let mut member = selected.member_with_return(ret);
            member.params = params;
            return Some(ResolvedCall::Member(
                crate::symbol_resolver::ResolvedMember {
                    receiver: recv,
                    physical_params: selected.callable.physical_params.clone(),
                    context_args: Vec::new(),
                    ret,
                    member,
                    projected_return_hazard: selected.projected_return_hazard,
                    suspend: selected.flags.suspend,
                    origin: selected.callable.origin,
                },
            ));
        }
        None
    }

    pub(super) fn record_declaration_iterator_protocol(
        &mut self,
        iterable: ExprId,
        iterable_ty: Ty,
        declaration_scope: &[TypeName],
    ) -> Option<Ty> {
        let iterator =
            self.declaration_zero_arg_operator(iterable_ty, "iterator", declaration_scope)?;
        let iter_ty = iterator.ret();
        let has_next = self.declaration_zero_arg_operator(iter_ty, "hasNext", declaration_scope)?;
        if has_next.ret() != Ty::Boolean {
            return None;
        }
        let next = self.declaration_zero_arg_operator(iter_ty, "next", declaration_scope)?;
        let elem_ty = next.ret();
        self.iterator_protocols.insert(
            iterable,
            IteratorProtocolTarget {
                iterator: Box::new(iterator),
                has_next: Box::new(has_next),
                next: Box::new(next),
                iter_ty,
                elem_ty,
            },
        );
        Some(elem_ty)
    }
}
