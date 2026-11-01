//! A member call that no candidate accepts during signature evaluation.
//!
//! kotlinc resolves such a call to its one candidate whose parameters the arguments map onto,
//! even when an argument's type does not fit (`j.c(null)` against Java `c(@NotNull String)` and
//! `c(int, int)`): the argument mismatch is that candidate's diagnostic and the call still has the
//! candidate's result. The body check reports the mismatch. Signature evaluation only needs the
//! result, so it must not record "none of the following candidates is applicable" for a call the
//! body check resolves; that list stays for a call no candidate's shape accepts.

use super::*;

enum RejectedCandidateResult {
    Fixed(crate::fir::ResolvedTy),
    Unfixed(String),
    Invalid,
}

impl ProductionSignatureSemantics<'_> {
    /// The result of a member call that selection rejected, or the recorded failure. The call has
    /// a result only when exactly one candidate at the nearest scope rank maps every argument to a
    /// parameter, accepts the written type-argument arity, and has a semantic result fixed by its
    /// receiver, explicit type arguments, and mapped argument types. A nearer name-scope rung
    /// remains final when none of its declarations accepts that arity; recovery never reinterprets
    /// the call at a farther rung.
    pub(super) fn rejected_member_call(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        receiver: Ty,
        spelling: &str,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        type_arguments: &[Ty],
        trailing_lambda: bool,
        expected: Option<Ty>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::fir::ResolvedMemberCall, crate::fir::DiagnosticId> {
        let level = self.with_resolver(scope, |resolver| {
            Some(
                self.receiver_member_level_with_context_status_and_constraints(
                    scope,
                    resolver,
                    receiver,
                    spelling,
                    type_arguments,
                    expected,
                ),
            )
        })?;
        let level = level?;
        let mut candidates = level.callables.functions().to_vec();
        let mut candidate_sources = level.candidate_sources;
        let names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_owned))
            .collect::<Vec<_>>();
        let names = names
            .iter()
            .any(Option::is_some)
            .then_some(names.as_slice());
        // Context availability is applicability, not declaration existence. When the body
        // checker will diagnose an unavailable context, retain a unique declaration's fixed
        // result for signature inference rather than manufacturing an unresolved-member error.
        // Project only the source-visible value parameters; unlike an available context, this
        // projection contributes no type bindings to the result. Uniqueness is still decided over
        // the one combined declaration family: partitioning context applicability must not make
        // either side look uniquely recoverable on its own.
        let mut unavailable_context = level.unavailable_context;
        let mut recovery_candidates = candidates.clone();
        recovery_candidates.extend(
            unavailable_context
                .iter()
                .cloned()
                .map(Self::without_implicit_context),
        );
        let direct_rank = Self::nearest_mapped_candidate_rank(
            &recovery_candidates,
            arguments.len(),
            names,
            trailing_lambda,
        );
        if let Some(direct_rank) = direct_rank {
            candidates.retain(|candidate| Self::candidate_scope_rank(candidate) == direct_rank);
            unavailable_context
                .retain(|candidate| Self::candidate_scope_rank(candidate) == direct_rank);
            recovery_candidates
                .retain(|candidate| Self::candidate_scope_rank(candidate) == direct_rank);
        }
        // A rejected direct member and a rejected member extension still form one specificity
        // family only when the configured compiler version does. Older versions stop at a mapped
        // rejected member; all versions fall through to member extensions when no direct shape
        // maps. Signature recovery follows the same policy as body diagnostics.
        let direct_maps = direct_rank.is_some();
        if crate::diagnostic_wording::inapplicable_member_joins_extensions() || !direct_maps {
            let member_extensions =
                self.implicit_member_extension_candidate_sources(scope, receiver, spelling);
            let unprojected = member_extensions
                .iter()
                .map(|source| source.candidate.clone())
                .collect::<Vec<_>>();
            let (mut extension_candidates, mut extension_unavailable_context) = self
                .partition_implicit_context_candidates_with_constraints(
                    scope,
                    unprojected,
                    type_arguments,
                    expected,
                );
            let mut extension_recovery_candidates = extension_candidates.clone();
            extension_recovery_candidates.extend(
                extension_unavailable_context
                    .iter()
                    .cloned()
                    .map(Self::without_implicit_context),
            );
            let extension_rank = Self::nearest_mapped_candidate_rank(
                &extension_recovery_candidates,
                arguments.len(),
                names,
                trailing_lambda,
            );
            if let Some(extension_rank) = extension_rank {
                extension_candidates
                    .retain(|candidate| Self::candidate_scope_rank(candidate) == extension_rank);
                extension_unavailable_context
                    .retain(|candidate| Self::candidate_scope_rank(candidate) == extension_rank);
                extension_recovery_candidates
                    .retain(|candidate| Self::candidate_scope_rank(candidate) == extension_rank);
                // Dispatch inheritance/override priority applies to applicable call shapes. A
                // nearer owner whose same-named extension cannot map the written arguments must
                // not hide the inherited declaration that can map them.
                extension_recovery_candidates.retain(|candidate| {
                    Self::candidate_maps_value_arguments(
                        candidate,
                        arguments.len(),
                        names,
                        trailing_lambda,
                    )
                });
                let any_type_arity_matches =
                    extension_recovery_candidates.iter().any(|candidate| {
                        type_arguments.is_empty()
                            || type_arguments.len() == candidate.semantic_signature().formals.len()
                    });
                if any_type_arity_matches {
                    extension_recovery_candidates.retain(|candidate| {
                        type_arguments.is_empty()
                            || type_arguments.len() == candidate.semantic_signature().formals.len()
                    });
                }
                self.retain_member_extension_dispatch_priority(
                    scope,
                    &member_extensions,
                    &mut extension_recovery_candidates,
                );
                extension_candidates.retain(|candidate| {
                    extension_recovery_candidates.iter().any(|selected| {
                        super::receiver_member_level::same_candidate_declaration(
                            candidate, selected,
                        )
                    })
                });
                extension_unavailable_context.retain(|candidate| {
                    extension_recovery_candidates.iter().any(|selected| {
                        super::receiver_member_level::same_candidate_declaration(
                            candidate, selected,
                        )
                    })
                });
            }
            if direct_maps && extension_rank.is_some() {
                // 2.4.20 compares mapped rejected declarations across the member and extension
                // families before assigning the diagnostic owner. Dispatch-tower rank selected
                // the extension rung already; it is not comparable to the member family's scope
                // rank, so present both chosen families at the direct rung for specificity.
                let direct_rank = direct_rank.expect("a mapped direct family has a selected rank");
                for candidate in extension_candidates
                    .iter_mut()
                    .chain(&mut extension_unavailable_context)
                {
                    candidate.scope_rank = direct_rank.1;
                }
                candidates.extend(extension_candidates);
                unavailable_context.extend(extension_unavailable_context);
                recovery_candidates = candidates.clone();
                recovery_candidates.extend(
                    unavailable_context
                        .iter()
                        .cloned()
                        .map(Self::without_implicit_context),
                );
                candidate_sources.extend(member_extensions);
            } else if !direct_maps && extension_rank.is_some() {
                // Falling through leaves the failed member rung behind. Its lower numeric scope
                // rank must not mask a mapped extension on an outer implicit receiver.
                candidates = extension_candidates;
                unavailable_context = extension_unavailable_context;
                recovery_candidates = extension_recovery_candidates;
                candidate_sources = member_extensions;
            }
        }
        let available_count = candidates.len();
        if let Some((index, candidate, parameter_by_argument)) = self.sole_mapped_candidate(
            scope,
            receiver,
            &recovery_candidates,
            arguments,
            names,
            type_arguments.len(),
            trailing_lambda,
        ) {
            let candidate_source = candidate_sources.iter().find(|source| {
                super::receiver_member_level::same_candidate_declaration(
                    &source.candidate,
                    candidate,
                )
            });
            let candidate = self.determined_rejected_candidate(
                scope,
                receiver,
                candidate,
                candidate_source,
                type_arguments,
                expected,
                demand,
            )?;
            match self.rejected_candidate_result(
                scope,
                receiver,
                &candidate,
                arguments,
                &parameter_by_argument,
                type_arguments,
                expected,
            ) {
                RejectedCandidateResult::Fixed(ty) => {
                    let ty = if candidate.constructor.is_some() {
                        self.named_construction_result(
                            scope,
                            origin,
                            super::callable_references::AppliedSourceAlias::selected(
                                candidate.constructor_alias(),
                                type_arguments,
                            ),
                            ty.get(),
                            arguments.first().map(|argument| argument.ty.get()),
                            expected
                                .and_then(|expected| crate::fir::ResolvedTy::new(expected).ok()),
                        )?
                    } else {
                        ty
                    };
                    return Ok(crate::fir::ResolvedMemberCall {
                        ty: Some(ty),
                        declaration: None,
                    });
                }
                RejectedCandidateResult::Unfixed(formal) => {
                    return Err(self.record_unfixed_type_parameter(scope, origin, &formal));
                }
                RejectedCandidateResult::Invalid => {}
            }
            if let Some(index) = index.checked_sub(available_count) {
                return Err(self.record_unavailable_context_call(
                    scope,
                    origin,
                    &unavailable_context[index],
                ));
            }
        }
        if candidates.is_empty() {
            if let Some(candidate) = unavailable_context
                .iter()
                .min_by_key(|candidate| candidate.scope_rung.selection_key(candidate.scope_rank))
            {
                return Err(self.record_unavailable_context_call(scope, origin, candidate));
            }
        }
        Err(self.record_member_call_selection_failure(
            scope,
            origin,
            receiver,
            spelling,
            &candidates,
        ))
    }

    /// Replace a same-module callable's provisional result with the result owned by its demanded
    /// stable declaration before rejected-call recovery classifies that result. This is the same
    /// signature-graph dependency edge ordinary selected members use; recovery must not turn a
    /// still-pending expression body into an inapplicable call.
    fn determined_rejected_candidate(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        candidate: &crate::libraries::FunctionInfo,
        candidate_source: Option<&super::receiver_member_level::ReceiverCandidateSource>,
        type_arguments: &[Ty],
        expected: Option<Ty>,
        demand: &mut dyn FnMut(
            crate::fir::DeclarationId,
        )
            -> Result<crate::fir::ResolvedSignature, crate::fir::DiagnosticId>,
    ) -> Result<crate::libraries::FunctionInfo, crate::fir::DiagnosticId> {
        // `candidate` is the exact declaration shape already chosen from the available or
        // unavailable-context family. Do not project the source declaration through the receiver
        // tower a second time: signature demand may run after that transient tower has changed.
        // The unprojected source is retained only to recover bindings supplied by the contexts that
        // are present and to apply the demanded declaration result to its dispatch receiver.
        let mut determined = candidate.clone();
        let context_bindings =
            candidate_source.map_or_else(crate::symbol_resolver::GSigBinds::default, |source| {
                self.implicit_context_candidate_bindings(
                    scope,
                    &source.candidate,
                    type_arguments,
                    expected,
                )
            });
        if !determined.callable.ret.mentions_pending()
            && !determined.semantic_signature().ret.mentions_pending()
        {
            if let Some(generic) = determined.generic_sig.as_mut() {
                generic.ret =
                    crate::symbol_resolver::ty_subst_keep_unbound(generic.ret, &context_bindings);
            } else {
                determined.callable.ret = crate::symbol_resolver::ty_subst_keep_unbound(
                    determined.callable.ret,
                    &context_bindings,
                );
            }
            determined.ret.class = determined
                .ret
                .class
                .map(|ty| crate::symbol_resolver::ty_subst_keep_unbound(ty, &context_bindings));
            return Ok(determined);
        }
        let source_candidate = candidate_source
            .map(|source| &source.candidate)
            .unwrap_or(candidate);
        let declaration = source_candidate
            .stable_declaration
            .ok_or_else(Self::failure)?;
        let signature = demand(declaration)?;
        let member = source_candidate.member_with_return(signature.result.get());
        let specialization_receiver = candidate_source
            .and_then(|source| source.dispatch_receiver)
            .unwrap_or(receiver);
        let result =
            self.apply_dispatch_receiver(specialization_receiver, &member, signature.result.get());
        let result = crate::symbol_resolver::ty_subst_keep_unbound(result, &context_bindings);
        if determined.callable.ret.mentions_pending() {
            determined.callable.ret = result;
        }
        if let Some(generic) = determined.generic_sig.as_mut() {
            if generic.ret.mentions_pending() {
                generic.ret = result;
            }
        }
        if candidate_source.is_some() {
            return Ok(determined);
        }
        Ok(determined)
    }

    fn candidate_maps_value_arguments(
        candidate: &crate::libraries::FunctionInfo,
        argument_count: usize,
        argument_names: Option<&[Option<String>]>,
        trailing_lambda: bool,
    ) -> bool {
        super::super::call_argument_parameter_indices(
            argument_count,
            candidate.semantic_params().len(),
            argument_names,
            trailing_lambda,
            &candidate.call_sig,
        )
        .is_some()
    }

    fn candidate_scope_rank(candidate: &crate::libraries::FunctionInfo) -> (u8, u32) {
        (candidate.scope_rung.selection_rank(), candidate.scope_rank)
    }

    /// The nearest extension-tower rung containing a shape-mapped declaration. Unlike an ordinary
    /// name-scope family, a member-extension tower continues past a nearer rung whose declarations
    /// cannot map the written arguments.
    fn nearest_mapped_candidate_rank(
        candidates: &[crate::libraries::FunctionInfo],
        argument_count: usize,
        argument_names: Option<&[Option<String>]>,
        trailing_lambda: bool,
    ) -> Option<(u8, u32)> {
        candidates
            .iter()
            .filter(|candidate| {
                Self::candidate_maps_value_arguments(
                    candidate,
                    argument_count,
                    argument_names,
                    trailing_lambda,
                )
            })
            .map(Self::candidate_scope_rank)
            .min()
    }

    /// Retain the ordinary member-extension winner(s) among the shape-mapped declarations on one
    /// dispatch rung. This removes an inherited declaration hidden by its override before generic
    /// callable specificity, which deliberately knows nothing about dispatch depth.
    fn retain_member_extension_dispatch_priority(
        &self,
        scope: crate::fir::SignatureScope,
        sources: &[super::receiver_member_level::ReceiverCandidateSource],
        candidates: &mut Vec<crate::libraries::FunctionInfo>,
    ) {
        let eligible = sources
            .iter()
            .filter(|source| source.member_extension_priority.is_some())
            .filter(|source| {
                candidates.iter().any(|candidate| {
                    super::receiver_member_level::same_candidate_declaration(
                        &source.candidate,
                        candidate,
                    )
                })
            })
            .collect::<Vec<_>>();
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let oracle = crate::symbol_resolver::SourceOracle(&source);
        let maximal = super::super::member_extension_selection::maximal_member_extensions(
            &oracle,
            &eligible,
            |source| {
                source
                    .member_extension_priority
                    .expect("eligible member extension retains its dispatch priority")
            },
        );
        candidates.retain(|candidate| {
            maximal.iter().any(|&index| {
                super::receiver_member_level::same_candidate_declaration(
                    candidate,
                    &eligible[index].candidate,
                )
            })
        });
    }

    fn sole_mapped_candidate<'a>(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        candidates: &'a [crate::libraries::FunctionInfo],
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        argument_names: Option<&[Option<String>]>,
        type_argument_count: usize,
        trailing_lambda: bool,
    ) -> Option<(usize, &'a crate::libraries::FunctionInfo, Vec<usize>)> {
        let mut mapped = candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                let parameters = super::super::call_argument_parameter_indices(
                    arguments.len(),
                    candidate.semantic_params().len(),
                    argument_names,
                    trailing_lambda,
                    &candidate.call_sig,
                )?;
                let declared = candidate.semantic_params();
                let source_arguments = (0..arguments.len()).collect::<Vec<_>>();
                let omitted_defaults = crate::libraries::map_call_args(
                    &source_arguments,
                    argument_names,
                    &candidate.call_sig.param_names,
                    declared.len(),
                    candidate.call_sig.required,
                    &candidate.call_sig.param_defaults,
                    candidate.call_sig.vararg_index,
                    trailing_lambda,
                )
                .ok()?
                .iter()
                .filter(|argument| argument.is_none())
                .count();
                let shape = parameters
                    .iter()
                    .zip(arguments)
                    .map(|(&parameter, argument)| {
                        let ty = declared[parameter];
                        if candidate.call_sig.vararg_index == Some(parameter) && !argument.spread {
                            ty.array_read_elem().unwrap_or(ty)
                        } else {
                            ty
                        }
                    })
                    .collect::<Vec<_>>();
                let generic = candidate
                    .generic_sig
                    .as_ref()
                    .is_some_and(|signature| !signature.formals.is_empty());
                Some((
                    index,
                    candidate,
                    parameters,
                    shape,
                    omitted_defaults,
                    generic,
                ))
            })
            .collect::<Vec<_>>();
        if let Some(nearest_rank) = mapped
            .iter()
            .map(|(_, candidate, ..)| Self::candidate_scope_rank(candidate))
            .min()
        {
            mapped
                .retain(|(_, candidate, ..)| Self::candidate_scope_rank(candidate) == nearest_rank);
        }
        let any_type_arity_matches = mapped.iter().any(|(_, candidate, ..)| {
            type_argument_count == 0
                || type_argument_count == candidate.semantic_signature().formals.len()
        });
        if any_type_arity_matches {
            mapped.retain(|(_, candidate, ..)| {
                type_argument_count == 0
                    || type_argument_count == candidate.semantic_signature().formals.len()
            });
        }
        if mapped.len() > 1 {
            let dominated = mapped
                .iter()
                .enumerate()
                .filter_map(|(index, (_, _, _, shape, omitted, _))| {
                    mapped
                        .iter()
                        .any(|(_, _, _, other_shape, other_omitted, _)| {
                            other_shape == shape && other_omitted < omitted
                        })
                        .then_some(index)
                })
                .collect::<crate::fx_hash::HashSet<_>>();
            mapped = mapped
                .into_iter()
                .enumerate()
                .filter_map(|(index, candidate)| (!dominated.contains(&index)).then_some(candidate))
                .collect();
        }
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        crate::symbol_resolver::retain_most_specific_declarations(
            &source,
            Some(receiver),
            &mut mapped,
            |(_, candidate, _, shape, _, generic)| {
                (
                    candidate
                        .is_extension()
                        .then(|| candidate.semantic_receiver())
                        .flatten()
                        // A direct member's declaration receiver is the receiver being checked.
                        // Keeping that slot aligned with an extension receiver lets declaration
                        // specificity compare their mapped value parameters instead of treating
                        // them as shapes of different arity.
                        .or(Some(receiver)),
                    shape.as_slice(),
                    *generic,
                    Some(candidate.callable.owner),
                )
            },
        );
        match mapped.as_slice() {
            [(index, candidate, parameters, _, _, _)] => {
                Some((*index, *candidate, parameters.clone()))
            }
            _ => None,
        }
    }

    fn without_implicit_context(
        mut candidate: crate::libraries::FunctionInfo,
    ) -> crate::libraries::FunctionInfo {
        let mut signature = candidate.semantic_signature().into_owned();
        let context_count = candidate.context_count.min(signature.params.len());
        let visible = (context_count..signature.params.len()).collect::<Vec<_>>();
        signature.params = signature.params[context_count..].to_vec();
        candidate.call_sig = candidate.call_sig.select_parameters(&visible);
        candidate.generic_sig = Some(signature);
        candidate.context_count = 0;
        candidate
    }

    /// The candidate's semantic result under the same receiver, explicit type arguments, and
    /// mapped argument constraints ordinary generic selection consumes. An unfixed result retains
    /// the exact declaration type parameter whose inference failure the call site owns.
    fn rejected_candidate_result(
        &self,
        scope: crate::fir::SignatureScope,
        receiver: Ty,
        candidate: &crate::libraries::FunctionInfo,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        parameter_by_argument: &[usize],
        type_arguments: &[Ty],
        expected: Option<Ty>,
    ) -> RejectedCandidateResult {
        let signature = candidate.semantic_signature();
        if !type_arguments.is_empty() && type_arguments.len() != signature.formals.len() {
            // The body checker owns the explicit-arity diagnostic, but signature inference still
            // needs a result that is already fixed independently of those invalid arguments. Do
            // not bind a partial explicit list or infer an unfixed declaration result here.
            let semantic = signature.apply_return_policy(&*self.table.libraries, signature.ret);
            let result = candidate.ret.apply(semantic);
            return if result.mentions_ty_param() {
                RejectedCandidateResult::Invalid
            } else {
                crate::fir::ResolvedTy::new(result).map_or(
                    RejectedCandidateResult::Invalid,
                    RejectedCandidateResult::Fixed,
                )
            };
        }
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let mut bindings = crate::symbol_resolver::seeded_gsig_binds(&signature, type_arguments);
        if candidate.is_extension() {
            if let Some(declared_receiver) = signature.receiver {
                crate::symbol_resolver::unify_ty_from_symbols(
                    &source,
                    declared_receiver,
                    receiver,
                    &mut bindings,
                );
            }
        }
        let receiver_bindings = bindings.clone();
        let inferred = crate::symbol_resolver::infer_generic_call_bindings_from_symbols(
            &source,
            &signature,
            parameter_by_argument
                .iter()
                .zip(arguments)
                .filter_map(|(&parameter, argument)| {
                    candidate
                        .call_sig
                        .parameter_contributes_to_inference(parameter)
                        .then_some((
                            parameter,
                            argument.ty.get(),
                            argument.spread
                                || (candidate.call_sig.vararg_index == Some(parameter)
                                    && argument.name.is_some()),
                        ))
                }),
            candidate.call_sig.vararg_index,
        );
        crate::symbol_resolver::merge_call_argument_bindings(
            &source,
            &signature,
            type_arguments,
            &receiver_bindings,
            &mut bindings,
            inferred,
        );
        if let Some(expected_bindings) = expected.and_then(|expected| {
            crate::symbol_resolver::infer_generic_return_bindings_from_symbols(
                &source,
                &signature,
                expected,
                |actual, bound| {
                    crate::assignable::is_assignable(
                        &crate::assignable::TyCtx::new(),
                        &crate::symbol_resolver::SourceOracle(&source),
                        actual,
                        bound,
                    )
                },
            )
        }) {
            crate::symbol_resolver::merge_generic_upper_bindings(
                &signature,
                type_arguments,
                &mut bindings,
                expected_bindings,
                |actual, bound| {
                    crate::assignable::is_assignable(
                        &crate::assignable::TyCtx::new(),
                        &crate::symbol_resolver::SourceOracle(&source),
                        actual,
                        bound,
                    )
                },
            );
        }
        let specialized = crate::symbol_resolver::ty_subst_keep_unbound(signature.ret, &bindings);
        let semantic = signature.apply_return_policy(&*self.table.libraries, specialized);
        let result = candidate.ret.apply(semantic);
        if result.mentions_ty_param() {
            let Some(formal) = signature.formals.iter().find(|formal| {
                crate::types::ty_mentions_param(result, std::slice::from_ref(*formal))
            }) else {
                return RejectedCandidateResult::Invalid;
            };
            return RejectedCandidateResult::Unfixed(
                crate::types::type_parameter_source_name(formal).to_string(),
            );
        }
        crate::fir::ResolvedTy::new(result).map_or(
            RejectedCandidateResult::Invalid,
            RejectedCandidateResult::Fixed,
        )
    }

    fn record_unfixed_type_parameter(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        formal: &str,
    ) -> crate::fir::DiagnosticId {
        self.record_source_diagnostic(
            scope.owner,
            origin,
            format!("cannot infer type for type parameter '{formal}'. Specify it explicitly."),
        )
    }

    fn record_unavailable_context_call(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        candidate: &crate::libraries::FunctionInfo,
    ) -> crate::fir::DiagnosticId {
        let signature = candidate.semantic_signature();
        let context_count = candidate.context_count.min(signature.params.len());
        let module = crate::module_symbols::ModuleSymbols::for_file(self.table, scope.source.raw());
        let source = crate::symbol_source::CompositeSource::new(vec![
            &module as &dyn crate::symbol_source::SymbolSource,
            &*self.table.libraries as &dyn crate::symbol_source::SymbolSource,
        ]);
        let receivers = self.implicit_receivers(scope);
        let missing =
            signature.params[..context_count]
                .iter()
                .enumerate()
                .find(|(_, parameter)| {
                    super::super::context_argument_types(
                        &receivers,
                        std::slice::from_ref(parameter),
                        &crate::symbol_resolver::SourceOracle(&source),
                    )
                    .is_none()
                });
        let Some((parameter, ty)) = missing else {
            return self.record_source_diagnostic(
                scope.owner,
                origin,
                "no implicit value is available for the context parameters".to_string(),
            );
        };
        let name = candidate
            .call_sig
            .param_names
            .get(parameter)
            .filter(|name| !name.is_empty())
            .map(String::as_str)
            .unwrap_or("context");
        self.record_source_diagnostic(
            scope.owner,
            origin,
            format!(
                "no context argument for '{name}: {}' found.",
                ty.source_name()
            ),
        )
    }

    /// The ambiguity the body check reports for the same call, so both phases publish one
    /// diagnostic.
    pub(super) fn record_ambiguous_call(
        &self,
        scope: crate::fir::SignatureScope,
        origin: crate::fir::OriginId,
        spelling: &str,
        candidates: &std::cell::RefCell<Vec<crate::libraries::FunctionInfo>>,
    ) -> crate::fir::DiagnosticId {
        let message =
            super::super::Checker::ambiguous_candidates_message(spelling, &*candidates.borrow());
        self.record_source_diagnostic(scope.owner, origin, message)
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
        candidates: &[crate::libraries::FunctionInfo],
    ) -> crate::fir::DiagnosticId {
        if candidates.is_empty() {
            self.record_unresolved_member(scope, origin, receiver, spelling)
        } else {
            self.record_inapplicable_member_call(scope.owner, origin, spelling, candidates)
        }
    }
}
