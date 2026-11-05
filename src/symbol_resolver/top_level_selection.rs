//! Overload selection over one top-level callable family.
//!
//! A family is selected under one argument list when every declaration maps the source arguments
//! to the same parameters; otherwise each declaration carries its own mapping and the family is
//! compared in source-argument order ([`super::separately_mapped_selection`]). Both forms run
//! applicability, `@LowPriorityInOverloadResolution`, integer-literal adaptation and specificity.

use super::declaration_specificity::discriminate_generics;
use super::overload_selection::low_priority_in_overload_resolution;
use super::separately_mapped_selection::{select_separately_mapped, SeparatelyMapped};
use super::*;

/// One declaration instantiated for the call under its own arguments.
struct Instantiated<'a> {
    function: &'a FunctionInfo,
    args: &'a [CallArgKind],
    call: Option<&'a CandidateCall>,
    params: Vec<Ty>,
    ret: Ty,
    bindings: GSigBinds,
}

/// A top-level family's selection. An ambiguous one keeps the tied declarations, in family order,
/// so the ambiguity can name them.
pub(crate) enum TopLevelSelection<T> {
    None,
    Selected(T),
    Ambiguous(Vec<FunctionInfo>),
}

impl SymbolResolver<'_> {
    /// Select from one top-level family under `arguments`. Distinguishes a call no declaration
    /// accepts from an ambiguous one.
    pub(crate) fn select_top_level_candidates(
        &self,
        name: &str,
        candidates: Vec<FunctionInfo>,
        arguments: FamilyArguments<'_>,
        type_args: &[Ty],
        expected: Option<Ty>,
        include_invisible: bool,
    ) -> TopLevelSelection<(FunctionInfo, LibraryCallable)> {
        let functions = FunctionSet {
            overloads: candidates,
        };
        let (selected, callable) = match self.pick_top_level_with_visibility(
            name,
            &functions,
            arguments,
            type_args,
            expected,
            include_invisible,
        ) {
            TopLevelSelection::Selected(selected) => selected,
            TopLevelSelection::None => return TopLevelSelection::None,
            TopLevelSelection::Ambiguous(tied) => return TopLevelSelection::Ambiguous(tied),
        };
        if let Some(selected) = selected {
            return TopLevelSelection::Selected((selected, callable));
        }
        // A default bridge is a provider-owned physical callable rather than one of the projected
        // source candidates above: it carries its realization's coordinates (`HttpClient$default`
        // and the bridge descriptor), so pair it back to the base declaration whose realization it
        // is. An ordinary selection carries its exact candidate identity out of the overload
        // engine and still matches by its own coordinates.
        let selected = functions.top_level().find(|candidate| {
            candidate.callable.owner == callable.owner
                && candidate.callable.name == callable.name
                && candidate.callable.descriptor == callable.descriptor
                || (callable.default_call
                    && candidate
                        .callable
                        .default_realization
                        .as_deref()
                        .is_some_and(|realization| {
                            realization.owner == callable.owner
                                && realization.name == callable.physical_name()
                                && realization.descriptor == callable.descriptor
                        }))
        });
        match selected {
            Some(selected) => TopLevelSelection::Selected((selected.clone(), callable)),
            None => TopLevelSelection::None,
        }
    }

    pub(super) fn pick_top_level_with_visibility(
        &self,
        name: &str,
        fs: &FunctionSet,
        arguments: FamilyArguments<'_>,
        type_args: &[Ty],
        expected: Option<Ty>,
        include_invisible: bool,
    ) -> TopLevelSelection<(Option<FunctionInfo>, LibraryCallable)> {
        let candidates: Vec<(&FunctionInfo, &[CallArgKind], Option<&CandidateCall>)> =
            match &arguments {
                FamilyArguments::Shared(args) => fs.top_level().map(|o| (o, *args, None)).collect(),
                FamilyArguments::PerCandidate { calls, .. } => fs
                    .overloads
                    .iter()
                    .zip(calls.iter())
                    .filter(|(o, _)| o.kind == FnKind::TopLevel)
                    .map(|(o, call)| (o, call.args.as_slice(), Some(call)))
                    .collect(),
            };
        let mut parsed: Vec<Instantiated<'_>> = candidates
            .into_iter()
            .filter(|(candidate, ..)| {
                include_invisible || self.non_member_callable_accessible(candidate)
            })
            .filter(|(o, args, _)| top_level_exact_parameters_admit(o, args, type_args))
            .filter_map(|(o, args, call)| {
                self.instantiate(name, o, args, call, type_args, expected)
            })
            .collect();
        let fits = |p: &Ty, a: &CallArgKind| a.fits_parameter(self.lib, &self.src, *p);
        let adapts = |p: &Ty, a: &CallArgKind, _i: usize| a.adapts_integer_literal_to(*p);

        // Kotlin removes low-priority declarations only when an ordinary declaration is actually
        // applicable. Do that once, before specificity, so every exact/literal/default/vararg branch
        // below sees one coherent candidate tier.
        let applicable = |candidate: &Instantiated<'_>| {
            let Instantiated {
                function: o,
                args,
                params,
                ..
            } = candidate;
            fixed_parameter_shape(params, args, |_, parameter, argument| {
                fits(parameter, argument)
            })
            .is_some()
                || omitted_parameter_shape(params, args, |position, parameter, argument| {
                    fits(parameter, argument) || adapts(parameter, argument, position)
                })
                .is_some()
                || candidate_vararg_shape(o, params, args, |position, parameter, argument| {
                    fits(parameter, argument) || adapts(parameter, argument, position)
                })
                .is_some()
        };
        let low_priority =
            |candidate: &Instantiated<'_>| low_priority_in_overload_resolution(candidate.function);
        if parsed
            .iter()
            .any(|candidate| !low_priority(candidate) && applicable(candidate))
        {
            parsed.retain(|candidate| !low_priority(candidate));
        }

        let pick = match arguments {
            FamilyArguments::Shared(args) => {
                match self.pick_shared(&parsed, args, &fits, &adapts) {
                    CandidateSelectionWithTies::Selected(pick) => Some(pick),
                    CandidateSelectionWithTies::Ambiguous(tied) => return tied_declarations(tied),
                    CandidateSelectionWithTies::None => {
                        let arg_tys: Vec<Ty> = args.iter().map(CallArgKind::ty).collect();
                        if let Some(c) = self
                            .select_top_level_default_callable(name, &arg_tys, type_args, expected)
                        {
                            crate::trace_compiler!(
                                "resolve",
                                "top-level {name} args={arg_tys:?} -> {}.{}{} default inline={:?}",
                                c.owner.render(),
                                c.name,
                                c.descriptor,
                                c.inline
                            );
                            return TopLevelSelection::Selected((None, c));
                        }
                        None
                    }
                }
            }
            FamilyArguments::PerCandidate { source, .. } => {
                let entries = parsed
                    .iter()
                    .map(|entry| SeparatelyMapped {
                        function: entry.function,
                        params: &entry.params,
                        args: entry.args,
                        call: entry
                            .call
                            .expect("a separately mapped family carries every declaration's call"),
                    })
                    .collect::<Vec<_>>();
                match select_separately_mapped(
                    &self.src,
                    &entries,
                    source,
                    self.eager_lambda_results,
                    &fits,
                    &adapts,
                ) {
                    CandidateSelectionWithTies::Selected(pick) => Some(&parsed[pick]),
                    CandidateSelectionWithTies::Ambiguous(tied) => {
                        return tied_declarations(
                            tied.into_iter().map(|pick| &parsed[pick]).collect(),
                        )
                    }
                    CandidateSelectionWithTies::None => None,
                }
            }
        };
        let Some(Instantiated {
            function: o,
            args,
            params,
            ret,
            bindings,
            ..
        }) = pick
        else {
            return TopLevelSelection::None;
        };
        let c = &o.callable;
        let mut vararg_elem = None;
        let ret_ty = o
            .generic_sig
            .as_ref()
            .map(|gsig| {
                vararg_elem = instantiate_vararg_element(&self.src, gsig, params, args, bindings);
                instantiate_signature_result(
                    &self.src,
                    gsig,
                    bindings,
                    UnboundSpecialization::UseUpperBound,
                )
            })
            .unwrap_or(*ret);
        // The signature owns post-substitution behavior, so every provider and callable origin
        // reaches the same realization path after ordinary inference. A suspend callable's physical
        // `Object` result is not its source return; its generic metadata remains authoritative too.
        let ret_ty = o.ret.apply(
            o.generic_sig
                .as_ref()
                .map_or(ret_ty, |sig| sig.apply_return_policy(self.lib, ret_ty)),
        );

        crate::trace_compiler!(
            "resolve",
            "top-level {name} args={:?} -> {}.{}{} inline={:?}",
            args.iter().map(CallArgKind::ty).collect::<Vec<_>>(),
            c.owner.render(),
            c.name,
            c.descriptor,
            c.inline
        );
        let callable = LibraryCallable {
            // A function with leading CONTEXT parameters emits the FULL parameter list (context
            // included) so the lowerer can prepend the implicit context sources; the matched
            // `params` were value-only for arity purposes. Without context, the matched list is
            // the full list (platform-nullability applied).
            params: if o.context_count > 0 {
                c.params.clone()
            } else {
                params.clone()
            },
            ret: ret_ty,
            physical_ret: *ret,
            default_call: false,
            vararg_elem,
            vararg_index: vararg_elem.and(o.call_sig.vararg_index),
            ..c.clone()
        };
        TopLevelSelection::Selected((Some((*o).clone()), callable))
    }

    /// Infer one declaration's type arguments from its own arguments and instantiate its value
    /// parameters. `None` when the inferred bindings break a bound.
    fn instantiate<'a>(
        &self,
        name: &str,
        o: &'a FunctionInfo,
        args: &'a [CallArgKind],
        call: Option<&'a CandidateCall>,
        type_args: &[Ty],
        expected: Option<Ty>,
    ) -> Option<Instantiated<'a>> {
        // Exact/default probes see ordinary runtime types; `args` separately drives adaptation.
        let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
        let semantic = o.semantic_signature();
        let mut bindings = seeded_gsig_binds(&semantic, type_args);
        let value_count = semantic.params.len().saturating_sub(o.context_count);
        let inferred = infer_generic_call_bindings_from_symbols(
            &self.src,
            &semantic,
            args.iter()
                .enumerate()
                .filter_map(|(argument_index, argument)| {
                    if argument.is_omitted_default() {
                        return None;
                    }
                    let value_parameter = match o.call_sig.vararg_index {
                        Some(vararg)
                            if argument_index >= vararg.saturating_sub(o.context_count) =>
                        {
                            vararg.saturating_sub(o.context_count)
                        }
                        _ if argument_index < value_count => argument_index,
                        _ => return None,
                    };
                    let parameter = o.context_count + value_parameter;
                    let declared = *semantic.params.get(parameter)?;
                    let whole_array = argument.is_spread();
                    let expected = if o.call_sig.vararg_index == Some(parameter) && !whole_array {
                        declared.array_read_elem().unwrap_or(declared)
                    } else {
                        declared
                    };
                    Some((
                        parameter,
                        argument.inference_type(&self.src, expected),
                        whole_array,
                    ))
                }),
            o.call_sig.vararg_index,
        );
        for (formal, actual) in inferred {
            bindings.entry(formal).or_insert(actual);
        }
        if let Some(expected) = expected {
            if let Some(inferred_return) = infer_generic_return_bindings_from_symbols(
                &self.src,
                &semantic,
                expected,
                |actual, bound| resolution_subtype(&self.src, actual, bound),
            ) {
                for (formal, actual) in inferred_return {
                    bindings.entry(formal).or_insert(actual);
                }
            }
        }
        if !generic_bindings_satisfy_bounds(&semantic, &bindings, |actual, bound| {
            resolution_subtype(&self.src, actual, bound)
        }) {
            crate::trace_compiler!(
                "resolve",
                "reject top-level candidate {}: bindings={:?}, bounds={:?}",
                name,
                bindings,
                semantic.formal_bounds
            );
            return None;
        }
        let params = apply_platform_call_parameter_nullability(
            instantiate_signature_parameters(
                &self.src,
                &semantic,
                &bindings,
                UnboundSpecialization::Preserve,
            ),
            &o.call_sig.platform_nullable_params,
            &arg_tys,
            o.call_sig.vararg,
        );
        // Leading CONTEXT parameters are supplied implicitly by the caller, never
        // positionally — matching and arity checks see only the value parameters.
        let value_params = params[o.context_count.min(params.len())..].to_vec();
        Some(Instantiated {
            function: o,
            args,
            call,
            params: value_params,
            ret: o.callable.ret,
            bindings,
        })
    }

    /// A family under one shared mapping: an exact match, then integer-literal adaptation, then
    /// the most specific fixed shape, then the most specific vararg shape, parameter by parameter.
    fn pick_shared<'p, 'a>(
        &self,
        parsed: &'p [Instantiated<'a>],
        args: &[CallArgKind],
        fits: &dyn Fn(&Ty, &CallArgKind) -> bool,
        adapts: &dyn Fn(&Ty, &CallArgKind, usize) -> bool,
    ) -> CandidateSelectionWithTies<&'p Instantiated<'a>> {
        let exact = |candidate: &&Instantiated<'_>| {
            candidate.params.len() == args.len()
                && candidate
                    .params
                    .iter()
                    .zip(args)
                    .all(|(parameter, argument)| *parameter == argument.type_for(*parameter))
        };
        // A generic declaration the call instantiates to a non-generic one's parameters is the
        // less specific of the two (`discriminate_generics`).
        if let Some(exact) = parsed
            .iter()
            .filter(exact)
            .find(|candidate| !is_generic(candidate.function))
            .or_else(|| parsed.iter().find(exact))
        {
            return CandidateSelectionWithTies::Selected(exact);
        }
        let generic = |candidate: &Instantiated<'_>| is_generic(candidate.function);
        match discriminate_generics(
            integer_literal_overload_with_ties(
                parsed.iter().map(|entry| (entry.params.clone(), entry)),
                args,
                |_, param, arg| fits(param, arg),
                |_position, left, right, arg| {
                    parameter_at_least_as_specific(&self.src, left, right, arg)
                        || resolution_subtype(&self.src, left, right)
                },
                |_, _| false,
            ),
            generic,
        ) {
            CandidateSelectionWithTies::None => {}
            selection => return selection,
        }
        match discriminate_generics(
            unique_most_specific_with_conflicts_and_ties(
                parsed.iter().filter_map(|entry| {
                    fixed_parameter_shape(&entry.params, args, |_, param, arg| fits(param, arg))
                        .map(|shape| (shape, entry))
                }),
                |_, left, right| resolution_subtype(&self.src, left, right),
                |_, _| false,
            ),
            generic,
        ) {
            CandidateSelectionWithTies::None => {}
            selection => return selection,
        }
        discriminate_generics(
            unique_most_specific_with_conflicts_and_ties(
                parsed.iter().filter_map(|entry| {
                    candidate_vararg_shape(entry.function, &entry.params, args, |i, param, arg| {
                        fits(param, arg) || adapts(param, arg, i)
                    })
                    .map(|shape| (shape, entry))
                }),
                |_, left, right| resolution_subtype(&self.src, left, right),
                |_, _| false,
            ),
            generic,
        )
    }
}

/// Whether a declaration declares type parameters of its own.
fn is_generic(function: &FunctionInfo) -> bool {
    !function.semantic_signature().formals.is_empty()
}

/// The declarations an ambiguous selection left tied.
fn tied_declarations<T>(tied: Vec<&Instantiated<'_>>) -> TopLevelSelection<T> {
    TopLevelSelection::Ambiguous(
        tied.into_iter()
            .map(|entry| entry.function.clone())
            .collect(),
    )
}
