//! Instantiating ONE selected callable against the call that chose it.
//!
//! Selection answers WHICH callable wins. This module answers what that callable then IS at this
//! call site: the parameter vector its arguments are checked against, the result type, and the
//! applied receiver the recorded target carries. They come from a single binding set on purpose —
//! if the parameters were instantiated separately from the result, a formal shared between them
//! could be bound one way for the argument expectations and another for the recorded call.
//!
//! The call's EXPECTED RESULT participates here for that reason: it is an inference input beside the
//! receiver and the arguments, seeded first so the receiver still fixes whatever it leaves open.
//!
//! Extracted from the resolver root, which is migration debt that may not grow.

use super::*;

impl<'a> SymbolResolver<'a> {
    /// Delegate conventions additionally need the receiver application inferred by their ordinary
    /// value arguments. For example, `D("K")` may initially have the raw type `D<>`, while
    /// `getValue(thisRef: R, ...)` fixes the owning `D<R>` to `D<String>`. Keep that application
    /// beside the selected declaration so the delegate initializer can be checked authoritatively
    /// against it; FIR must never reconstruct the inference.
    pub(crate) fn select_receiver_function_with_applied_receiver_tracking(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        callables: &Callables,
        expected_result: Option<Ty>,
    ) -> ReceiverFunctionSelection {
        let selected = match select_receiver_overload_from_functions_tracking(
            self.lib,
            receiver,
            name,
            args,
            type_args,
            ExtCtx {
                fn_scope: self.fn_scope,
                source: &self.src,
            },
            callables.functions(),
            IndexedConvention::Ordinary,
        ) {
            CandidateSelectionWithTies::Selected(selected) => selected,
            CandidateSelectionWithTies::None => return ReceiverFunctionSelection::None,
            CandidateSelectionWithTies::Ambiguous(candidates) => {
                return ReceiverFunctionSelection::Ambiguous(candidates)
            }
        };
        let binding_receiver = selected
            .semantic_receiver()
            .and_then(|declared| {
                ReceiverMro::new(&self.src, receiver).binding_receiver(&self.src, declared)
            })
            .unwrap_or(receiver);
        let semantic = selected.semantic_signature();
        let mut bindings = seeded_gsig_binds(&semantic, type_args);
        // The call's EXPECTED RESULT is an inference input beside the receiver and the arguments.
        // It is seeded into THIS binding set — the one that produces the parameter vector, the
        // result type and the recorded target together — so the call stays one coherent decision. A
        // formal shared between the result and the operands cannot then be bound one way for the
        // argument expectations and another way for the recorded result.
        //
        // Seeded FIRST: the receiver still fixes whatever the expectation leaves open, and an
        // explicit type argument (already in `bindings`) still wins. An ARGUMENT still cannot widen
        // a receiver-fixed formal — that rule is below and unchanged.
        if let Some(expected) = expected_result {
            if let Some(inferred) = infer_generic_return_bindings_from_symbols(
                &self.src,
                &semantic,
                expected,
                |actual, bound| resolution_subtype(&self.src, actual, bound),
            ) {
                merge_generic_upper_bindings(
                    &semantic,
                    type_args,
                    &mut bindings,
                    inferred,
                    |actual, bound| resolution_subtype(&self.src, actual, bound),
                );
            }
        }
        if let Some(declared_receiver) = semantic.receiver {
            unify_ty(declared_receiver, binding_receiver, &mut bindings);
            // A caller-owned symbolic receiver is a real binding fact. Extension result
            // specialization used to preserve it in a second, return-only binder; retain it in
            // the one call-site binding set instead.
            preserve_receiver_identity_bindings(declared_receiver, binding_receiver, &mut bindings);
        }
        let receiver_bindings = bindings.clone();
        let value_params = &semantic.params[selected.context_count.min(semantic.params.len())..];
        let mut argument_bindings = GSigBinds::new();
        for (index, (&parameter, argument)) in value_params.iter().zip(args).enumerate() {
            let parameter_index = selected.context_count + index;
            if selected
                .call_sig
                .parameter_contributes_to_inference(parameter_index)
                // A nested generic call reaches here as a provisional, which is normally not
                // evidence. It IS evidence when its own inputs fix its result — `xs.map { B(it) }`
                // binds `R` from the transform it was given — and without it `a + b + c` fails
                // where `a + b` does not: the inner sum arrives as a provisional, contributes
                // nothing, and the shared `T` of `Iterable<T>.plus(Iterable<T>)` stays pinned at
                // the receiver's element type.
                && (argument.contributes_type_to_inference()
                    || argument.result_is_input_constrained())
            {
                unify_inferred_ty_with_source(
                    &self.src,
                    parameter,
                    argument.inference_type(&self.src, parameter),
                    &mut argument_bindings,
                );
            }
        }
        let owner_argument_bindings = argument_bindings.clone();
        merge_call_argument_bindings(
            &self.src,
            &semantic,
            type_args,
            &receiver_bindings,
            &mut bindings,
            argument_bindings,
        );
        // A raw owning classifier has no receiver arguments to seed its declaration parameters.
        // They are nevertheless ordinary inference variables when a convention parameter mentions
        // them (`D<in R>.getValue(thisRef: R, ...)`). Method generic signatures intentionally list
        // only method-owned formals, so retain the argument constraints for the owner's distinct
        // stable formals here.
        if let Ty::Obj(owner, arguments) = receiver.non_null() {
            if arguments.is_empty() {
                if let Some(classifier) = self.src.classifier(owner) {
                    for formal in &classifier.type_params {
                        if let Some(inferred) = owner_argument_bindings.get(formal).copied() {
                            bindings.entry(formal.clone()).or_insert(inferred);
                        }
                    }
                }
            }
        }
        if selected.is_extension() || expected_result.is_some() {
            complete_bottom_constraint_bindings(&semantic, &mut bindings, type_args.len());
        }
        crate::trace_compiler!(
            "fir",
            "receiver call application receiver={receiver:?} name={name} semantic={semantic:?} bindings={bindings:?}",
        );
        let params = value_params
            .iter()
            .map(|parameter| ty_subst_keep_unbound(*parameter, &bindings))
            .collect::<Vec<_>>();
        let applied_receiver = match receiver.non_null() {
            Ty::Obj(owner, arguments) => self
                .src
                .classifier(owner)
                .and_then(|classifier| {
                    let applied = if arguments.len() == classifier.type_params.len() {
                        arguments
                            .iter()
                            .map(|argument| ty_subst_keep_unbound(*argument, &bindings))
                            .collect::<Vec<_>>()
                    } else if arguments.is_empty() {
                        classifier
                            .type_params
                            .iter()
                            .map(|formal| bindings.get(formal).copied())
                            .collect::<Option<Vec<_>>>()?
                    } else {
                        return None;
                    };
                    Some(Ty::obj_args_name(owner, &applied))
                })
                .unwrap_or(receiver),
            _ => receiver,
        };
        let ret = if selected.is_extension() {
            let inferred =
                specialize_final_signature_output_type(&self.src, semantic.ret, &bindings);
            specialized_extension_return(self.lib, &selected, inferred)
        } else {
            let provider = resolved_member_from_info(
                self.lib,
                &self.src,
                receiver,
                args,
                type_args,
                selected.clone(),
            )
            .ret;
            let inferred = if expected_result.is_some() {
                specialize_final_signature_output_type(&self.src, semantic.ret, &bindings)
            } else {
                ty_subst_keep_unbound(semantic.ret, &bindings)
            };
            merge_specialized_return(provider, inferred)
        };
        ReceiverFunctionSelection::Selected((Box::new(selected), params, ret, applied_receiver))
    }
}
