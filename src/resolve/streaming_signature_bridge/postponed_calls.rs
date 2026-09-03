//! Candidate projection for builder-inference calls inside contextual function literals.
//!
//! The compact graph owns only the active constraint variables. Candidate collection, argument
//! mapping, overload selection, and generic inference remain in `SymbolResolver`; this adapter
//! substitutes constraints learned from typed arguments into each candidate before invoking that
//! ordinary selector, then exposes only the selected declaration's constraints for commitment.

use super::*;

pub(super) struct PostponedCallableFamily {
    callables: crate::libraries::Callables,
    bindings: Vec<(
        crate::libraries::FunctionInfo,
        crate::symbol_resolver::GSigBinds,
    )>,
}

impl PostponedCallableFamily {
    pub(super) fn callables(&self) -> &crate::libraries::Callables {
        &self.callables
    }

    pub(super) fn selected_bindings(
        &self,
        selected: &crate::libraries::FunctionInfo,
    ) -> crate::symbol_resolver::GSigBinds {
        self.bindings
            .iter()
            .find(|(candidate, _)| same_callable(candidate, selected))
            .map(|(_, bindings)| bindings.clone())
            .unwrap_or_default()
    }
}

fn same_callable(
    left: &crate::libraries::FunctionInfo,
    right: &crate::libraries::FunctionInfo,
) -> bool {
    left.stable_declaration == right.stable_declaration
        && left.source_member == right.source_member
        && left.source_key == right.source_key
        && left.callable.owner == right.callable.owner
        && left.callable.name == right.callable.name
        && left.callable.descriptor == right.callable.descriptor
}

pub(super) fn collect_type_parameters(
    ty: Ty,
    parameters: &mut std::collections::HashSet<&'static str>,
) {
    match ty {
        Ty::TyParam(name, _) => {
            parameters.insert(name);
        }
        Ty::Nullable(inner)
        | Ty::PlatformNullable(inner)
        | Ty::InProjection(inner)
        | Ty::OutProjection(inner) => collect_type_parameters(*inner, parameters),
        // A star's upper bound is a computed capture fact, not a source occurrence that can
        // constrain a postponed call's type parameters.
        Ty::StarProjection(_) => {}
        Ty::Obj(_, arguments) => {
            for argument in arguments {
                collect_type_parameters(*argument, parameters);
            }
        }
        Ty::Fun(signature) => {
            for parameter in &signature.params {
                collect_type_parameters(*parameter, parameters);
            }
            collect_type_parameters(signature.ret, parameters);
        }
        _ => {}
    }
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn common_postponed_parameters(
        &self,
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        shapes: Vec<Vec<Ty>>,
    ) -> Option<Vec<Ty>> {
        if shapes.is_empty() || shapes.iter().any(|shape| shape.len() != arguments.len()) {
            return None;
        }
        let mut common = Vec::with_capacity(arguments.len());
        for (index, argument) in arguments.iter().enumerate() {
            if matches!(argument, crate::fir::SigCallArgumentProbe::Typed(_)) {
                common.push(Ty::obj("kotlin/Any"));
                continue;
            }
            let functions = shapes
                .iter()
                .map(|shape| resolver.functional_expectation(shape[index]))
                .collect::<Option<Vec<_>>>()?;
            let Ty::Fun(first) = functions.first()?.non_null() else {
                return None;
            };
            let same_inputs = functions.iter().all(|function| {
                matches!(function.non_null(), Ty::Fun(signature)
                    if signature.params == first.params
                        && signature.context_count == first.context_count
                        && signature.has_receiver == first.has_receiver
                        && signature.suspend == first.suspend)
            });
            if !same_inputs {
                return None;
            }
            let same_result = functions.iter().all(
                |function| matches!(function.non_null(), Ty::Fun(signature) if signature.ret == first.ret),
            );
            if !same_result
                && matches!(
                    argument,
                    crate::fir::SigCallArgumentProbe::PostponedCallableReference { .. }
                )
            {
                return None;
            }
            let result = if same_result {
                first.ret
            } else {
                Ty::ty_param(
                    "\0sig:common-lambda-result",
                    Ty::nullable(Ty::obj("kotlin/Any")),
                )
            };
            common.push(Ty::fun_with_shape(
                first.params.clone(),
                result,
                first.context_count,
                first.has_receiver,
                first.suspend,
            ));
        }
        Some(common)
    }

    pub(super) fn project_postponed_callables(
        &self,
        scope: crate::fir::SignatureScope,
        callables: crate::libraries::Callables,
        arguments: &[crate::symbol_resolver::CallArgKind],
    ) -> PostponedCallableFamily {
        let constraint_frame = self.active_scoped_constraint_frame(scope.owner);
        let mut active = std::collections::HashSet::new();
        if let Some(inputs) = constraint_frame.and_then(|(owner, index)| {
            self.scoped_constraint_inputs
                .borrow()
                .get(&owner)
                .and_then(|stack| stack.get(index))
                .cloned()
        }) {
            for input in inputs {
                collect_type_parameters(input, &mut active);
            }
        }
        if active.is_empty() {
            return PostponedCallableFamily {
                callables,
                bindings: Vec::new(),
            };
        }
        let known = constraint_frame
            .and_then(|(owner, index)| {
                self.scoped_constraints
                    .borrow()
                    .get(&owner)
                    .and_then(|stack| stack.get(index))
                    .cloned()
            })
            .unwrap_or_default();

        let (mut functions, properties) = callables.into_parts();
        let mut recorded = Vec::new();
        for candidate in &mut functions.overloads {
            let Some(mut signature) = candidate.generic_sig.clone() else {
                continue;
            };
            let value_parameters =
                &signature.params[candidate.context_count.min(signature.params.len())..];
            let mut bindings = known.clone();
            for (&parameter, argument) in value_parameters.iter().zip(arguments) {
                if argument.is_omitted_default() || argument.is_lambda_literal() {
                    continue;
                }
                crate::symbol_resolver::unify_inferred_ty(
                    parameter,
                    argument.type_for(parameter),
                    &mut bindings,
                );
            }
            bindings.retain(|formal, _| active.contains(formal.as_str()));
            if bindings.is_empty() {
                continue;
            }

            signature.receiver = signature
                .receiver
                .map(|receiver| crate::symbol_resolver::ty_subst_keep_unbound(receiver, &bindings));
            for parameter in &mut signature.params {
                *parameter = crate::symbol_resolver::ty_subst_keep_unbound(*parameter, &bindings);
            }
            signature.ret = crate::symbol_resolver::ty_subst_keep_unbound(signature.ret, &bindings);
            for bounds in &mut signature.formal_bounds {
                for bound in bounds {
                    *bound = crate::symbol_resolver::ty_subst_keep_unbound(*bound, &bindings);
                }
            }
            candidate.receiver = candidate
                .receiver
                .map(|receiver| crate::symbol_resolver::ty_subst_keep_unbound(receiver, &bindings));
            candidate.generic_sig = Some(signature);
            recorded.push((candidate.clone(), bindings));
        }
        PostponedCallableFamily {
            callables: crate::libraries::Callables::from_parts(functions, properties),
            bindings: recorded,
        }
    }

    pub(super) fn commit_postponed_bindings(
        &self,
        scope: crate::fir::SignatureScope,
        bindings: crate::symbol_resolver::GSigBinds,
    ) {
        if bindings.is_empty() {
            return;
        }
        let Some((constraint_owner, constraint_index)) =
            self.active_scoped_constraint_frame(scope.owner)
        else {
            return;
        };
        let mut constraints = self.scoped_constraints.borrow_mut();
        let Some(active) = constraints
            .get_mut(&constraint_owner)
            .and_then(|stack| stack.get_mut(constraint_index))
        else {
            return;
        };
        ProductionSignatureSemantics::merge_scoped_constraints(active, bindings);
    }
}
