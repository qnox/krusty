//! Complete the semantic result of an already-selected constructor.

use super::*;

fn constructor_inference_inputs(
    signature: &crate::libraries::GenericSig,
    vararg_index: Option<usize>,
    kinds: &[crate::symbol_resolver::CallArgKind],
    types: &[Ty],
) -> Option<Vec<(usize, Ty, bool)>> {
    if kinds.len() != types.len() {
        return None;
    }
    let expanded_vararg = types.len() > signature.params.len();
    let mut inputs = Vec::new();
    for (source, (kind, actual)) in kinds.iter().zip(types).enumerate() {
        let parameter = match vararg_index {
            Some(vararg) if expanded_vararg && source >= vararg => vararg,
            _ => source,
        };
        let declared = signature.params.get(parameter).copied()?;
        if !kind.is_omitted_default() && *actual != Ty::Error {
            let actual = if kind.is_integer_literal() {
                let inference_shape = if vararg_index == Some(parameter) && !kind.is_spread() {
                    declared.array_read_elem().unwrap_or(declared)
                } else {
                    declared
                };
                kind.type_for(inference_shape)
            } else {
                *actual
            };
            inputs.push((parameter, actual, kind.is_spread()));
        }
    }
    Some(inputs)
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn constructor_result(
        &self,
        scope: crate::fir::SignatureScope,
        selected: &crate::symbol_resolver::SelectedConstructorDeclaration,
        arguments: &[Ty],
        bound_outer: Option<Ty>,
        explicit_type_arguments: &[Ty],
        expected: Option<Ty>,
    ) -> Result<crate::fir::ResolvedTy, crate::fir::DiagnosticId> {
        use crate::symbol_resolver::selected_constructor::unlisted_constructor_type;
        let member = &selected.declaration;
        let owner = member.owner.ok_or_else(Self::failure)?;
        let Some(class) = self.table.class_by_type_name(owner) else {
            let result =
                unlisted_constructor_type(selected, owner, explicit_type_arguments, expected);
            return crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure());
        };
        // Constructor selection has already applied explicit arguments, literal adaptation,
        // generic bounds, and overload mapping to `member.ret`. Seed those selected classifier
        // arguments before filling any still-unbound variable from the raw expression types; doing
        // the latter first would incorrectly turn `C<T : Long>(0)` into `C<Int>`.
        let selected_arguments = (member.ret.obj_internal() == Some(owner))
            .then(|| member.ret.type_args())
            .unwrap_or_default();
        let mut bindings = class
            .type_params
            .iter()
            .cloned()
            .zip(selected_arguments.iter().copied())
            .collect::<crate::symbol_resolver::GSigBinds>();
        // A selected constructor result can contain an early partial solution (for example the
        // first `T` occurrence in `PairBox("x", null)`). It is not a completed classifier result
        // until every argument occurrence has contributed. Infer into a fresh map, then replace
        // only classifier slots not fixed by a written type argument; this preserves
        // `C<Number>(1)` while completing `PairBox("x", null)` to `PairBox<String?>`.
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        // Compact argument types complete postponed lambdas and nullability after selection, but
        // they are still one constructor constraint system. Reuse the ordinary variance- and
        // hierarchy-aware solver instead of pairwise unification: the latter widens
        // `C("x", StringBuilder())` to `C<Any>` and lets a contravariant `(Any) -> Unit` erase the
        // independent covariant `T = String` evidence.
        let inferred = member
            .generic_sig
            .as_ref()
            .and_then(|signature| {
                let inputs = constructor_inference_inputs(
                    signature,
                    member.call_sig.vararg_index,
                    &selected.arguments,
                    arguments,
                )?;
                Some(
                    crate::symbol_resolver::infer_generic_call_bindings_from_symbols(
                        &source,
                        signature,
                        inputs,
                        member.call_sig.vararg_index,
                    ),
                )
            })
            // A non-generic constructor has no bindings to add. A malformed generic declaration
            // whose retained call shape no longer aligns with its selected arguments is likewise
            // not repaired by pairwise fallback inference; the selected semantic result below
            // remains authoritative.
            .unwrap_or_default();
        let constructor_coordinates = member.generic_sig.as_ref().map(|signature| {
            crate::libraries::ConstructorTypeParameterCoordinates::new(
                &class.type_params,
                signature,
            )
        });
        let inferred_classifier = constructor_coordinates
            .as_ref()
            .map(|coordinates| coordinates.classifier_bindings(&inferred))
            .unwrap_or_else(|| inferred.clone());
        let mut completed_bindings = bindings.clone();
        for (index, formal) in class.type_params.iter().enumerate() {
            let explicitly_fixed = explicit_type_arguments
                .get(index)
                .is_some_and(|argument| *argument != Ty::Error);
            if !explicitly_fixed {
                if let Some(inferred) = inferred_classifier.get(formal).copied() {
                    completed_bindings.insert(formal.clone(), inferred);
                }
            }
        }
        // The fresh completion above sees compact expression result types, but no longer carries
        // literal provenance. It may therefore infer `Int` from the raw `0` after constructor
        // selection correctly adapted that literal to a declared `T : Long` parameter. Accept the
        // completed map only when it still satisfies the constructor's declaration bounds;
        // otherwise the selected semantic result remains authoritative. This is generic
        // constructor inference—value classes use the same path as ordinary classes.
        let completed_is_valid = member.generic_sig.as_ref().is_none_or(|signature| {
            let oracle = crate::symbol_resolver::SourceOracle(&source);
            let coordinates = constructor_coordinates
                .as_ref()
                .expect("a generic constructor has type-parameter coordinates");
            let mut signature_bindings = inferred.clone();
            signature_bindings.extend(coordinates.signature_bindings(&completed_bindings));
            crate::symbol_resolver::generic_bindings_satisfy_bounds(
                signature,
                &signature_bindings,
                |actual, bound| {
                    crate::assignable::is_assignable(
                        &crate::assignable::TyCtx::new(),
                        &oracle,
                        actual,
                        bound,
                    )
                },
            )
        });
        if completed_is_valid {
            bindings = completed_bindings;
        }
        let formal_bounds = class
            .type_param_bounds
            .iter()
            .map(|bound| {
                (*bound != Ty::Error)
                    .then_some(*bound)
                    .into_iter()
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        crate::symbol_resolver::seed_unbound_constructor_result_from_symbols(
            &source,
            owner,
            &class.type_params,
            &formal_bounds,
            expected,
            &mut bindings,
        );
        let mut result_arguments = class
            .type_params
            .iter()
            .enumerate()
            .map(|(index, formal)| {
                bindings.get(formal).copied().unwrap_or_else(|| {
                    class
                        .type_param_bounds
                        .get(index)
                        .copied()
                        .filter(|bound| *bound != Ty::Error)
                        .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                })
            })
            .collect::<Vec<_>>();
        if !class.captured_type_parameters.type_params.is_empty() {
            let receivers = bound_outer
                .into_iter()
                .chain(self.implicit_receivers(scope));
            let receiver_bindings = receivers
                .flat_map(|receiver| self.table.applied_type_hierarchy(receiver.non_null()))
                .filter_map(|(receiver_owner, applied, _)| {
                    self.table
                        .class_by_type_name(receiver_owner)
                        .map(|declaration| declaration.type_parameter_bindings(applied))
                })
                .collect::<Vec<_>>();
            for (ordinal, captured) in class
                .captured_type_parameters
                .type_params
                .iter()
                .enumerate()
            {
                let argument = receiver_bindings
                    .iter()
                    .find_map(|bindings| bindings.get(captured).copied())
                    .unwrap_or_else(|| {
                        Ty::ty_param(
                            captured,
                            class
                                .captured_type_parameters
                                .type_param_bounds
                                .get(ordinal)
                                .copied()
                                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any"))),
                        )
                    });
                result_arguments.push(argument);
            }
        }
        let result = Ty::obj_args_name(owner, &result_arguments);
        crate::fir::ResolvedTy::new(result).map_err(|_| Self::failure())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_inputs_preserve_vararg_element_and_spread_provenance() {
        let array = Ty::array(Ty::String);
        let signature = crate::libraries::GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params: vec![array],
            ret: Ty::Unit,
            return_policy: Default::default(),
        };
        let kinds = [
            crate::symbol_resolver::CallArgKind::Typed(Ty::String),
            crate::symbol_resolver::CallArgKind::Spread(array),
        ];

        assert_eq!(
            constructor_inference_inputs(&signature, Some(0), &kinds, &[Ty::String, array]),
            Some(vec![(0, Ty::String, false), (0, array, true)])
        );
    }
}
