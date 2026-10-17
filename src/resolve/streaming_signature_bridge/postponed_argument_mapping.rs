//! Argument mapping for a call whose postponed arguments the signature pass must shape.
//!
//! A postponed lambda or callable reference is probed by its declared shape only, after the
//! source arguments are mapped onto each candidate's parameters.
//!
//! The ordinary selector takes one parameter-ordered argument list, so candidates that map the
//! same source arguments onto different parameters (a trailing lambda filling `block` directly, or
//! filling it after a defaulted `n`) cannot be selected as one family. Each distinct argument
//! mapping is selected on its own by that selector. When every mapping yields a candidate and all
//! of them pass every argument to the same parameter type, they are equally specific and Kotlin
//! prefers the candidate whose call uses no default argument. Any other outcome stays undecided: a
//! family whose own selection is undecided may hold the call's candidate, and a postponed
//! argument's own shape may still rule a candidate out.

use super::*;

pub(super) struct MappedTopLevelSelection {
    pub(super) selected: crate::libraries::FunctionInfo,
    pub(super) value_parameters: Vec<Ty>,
    pub(super) kinds: Vec<crate::symbol_resolver::CallArgKind>,
    pub(super) slots: Vec<Option<usize>>,
}

impl MappedTopLevelSelection {
    /// The parameter type each source argument is passed to. Candidates that agree on these are
    /// equally specific, so only the default-argument tie-break can separate them.
    fn argument_parameters(&self, argument_count: usize) -> Vec<Option<Ty>> {
        (0..argument_count)
            .map(|source| {
                let parameter = self.slots.iter().position(|slot| *slot == Some(source))?;
                self.value_parameters.get(parameter).copied()
            })
            .collect()
    }
}

struct MappingFamily {
    candidates: Vec<crate::libraries::FunctionInfo>,
    kinds: Vec<crate::symbol_resolver::CallArgKind>,
    slots: Vec<Option<usize>>,
}

impl ProductionSignatureSemantics<'_> {
    pub(super) fn probe_call_arguments(
        candidates: &[crate::libraries::FunctionInfo],
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        trailing_lambda: bool,
    ) -> Option<(Vec<crate::symbol_resolver::CallArgKind>, Vec<Option<usize>>)> {
        if arguments.iter().any(|argument| {
            matches!(
                argument,
                crate::fir::SigCallArgumentProbe::PostponedLambda { spread: true, .. }
                    | crate::fir::SigCallArgumentProbe::PostponedCallableReference {
                        spread: true,
                        ..
                    }
            )
        }) {
            return None;
        }
        let names = arguments
            .iter()
            .map(|argument| match argument {
                crate::fir::SigCallArgumentProbe::Typed(argument) => {
                    argument.name.map(str::to_owned)
                }
                crate::fir::SigCallArgumentProbe::PostponedLambda { name, .. }
                | crate::fir::SigCallArgumentProbe::PostponedCallableReference { name, .. } => {
                    name.map(str::to_owned)
                }
            })
            .collect::<Vec<_>>();
        let requires_mapping = trailing_lambda || names.iter().any(Option::is_some);
        let extra_admitted = |first: usize, extra: usize| {
            Self::same_vararg_element_probe(&arguments[first], &arguments[extra])
        };
        let slots = if requires_mapping {
            Self::mapped_call_slots(
                candidates,
                &names,
                arguments.len(),
                trailing_lambda,
                &extra_admitted,
            )?
        } else {
            (0..arguments.len()).map(Some).collect()
        };
        let kinds = slots
            .iter()
            .map(|source| {
                source
                    .and_then(|source| arguments.get(source))
                    .map(Self::probe_argument_kind)
                    .unwrap_or(crate::symbol_resolver::CallArgKind::OmittedDefault)
            })
            .collect();
        Some((kinds, slots))
    }

    pub(super) fn select_mapped_top_level_candidate(
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        spelling: &str,
        candidates: &[crate::libraries::FunctionInfo],
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        trailing_lambda: bool,
        type_arguments: &[Ty],
    ) -> Option<MappedTopLevelSelection> {
        let families = Self::mapping_families(candidates, arguments, trailing_lambda)?;
        if let [family] = families.as_slice() {
            return Self::select_mapping_family(resolver, spelling, family, type_arguments)
                .or_else(|| Self::unique_visible_candidate(resolver, family, type_arguments));
        }
        // A family the ordinary selector leaves undecided may still hold the call's candidate, so
        // the shape is only known once every family has selected one.
        let mut selections = families
            .iter()
            .map(|family| Self::select_mapping_family(resolver, spelling, family, type_arguments))
            .collect::<Option<Vec<_>>>()?;
        let first = selections.first()?.argument_parameters(arguments.len());
        if selections
            .iter()
            .any(|selection| selection.argument_parameters(arguments.len()) != first)
        {
            return None;
        }
        if selections.len() > 1 {
            selections.retain(|selection| {
                !selection
                    .kinds
                    .iter()
                    .any(crate::symbol_resolver::CallArgKind::is_omitted_default)
            });
        }
        let [_] = selections.as_slice() else {
            return None;
        };
        selections.pop()
    }

    /// Candidates grouped by the parameter each source argument maps to. A call whose
    /// declarations all agree on that mapping is one family.
    fn mapping_families(
        candidates: &[crate::libraries::FunctionInfo],
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        trailing_lambda: bool,
    ) -> Option<Vec<MappingFamily>> {
        if let Some((kinds, slots)) =
            Self::probe_call_arguments(candidates, arguments, trailing_lambda)
        {
            return Some(vec![MappingFamily {
                candidates: candidates.to_vec(),
                kinds,
                slots,
            }]);
        }
        let mut families = Vec::<MappingFamily>::new();
        for candidate in candidates {
            let candidate = std::slice::from_ref(candidate);
            let (kinds, slots) = Self::probe_call_arguments(candidate, arguments, trailing_lambda)?;
            match families.iter_mut().find(|family| family.slots == slots) {
                Some(family) => family.candidates.extend_from_slice(candidate),
                None => families.push(MappingFamily {
                    candidates: candidate.to_vec(),
                    kinds,
                    slots,
                }),
            }
        }
        Some(families)
    }

    fn select_mapping_family(
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        spelling: &str,
        family: &MappingFamily,
        type_arguments: &[Ty],
    ) -> Option<MappedTopLevelSelection> {
        let (selected, _) = resolver.select_top_level_function_candidates(
            spelling,
            family.candidates.clone(),
            &family.kinds,
            type_arguments,
        )?;
        let parameters = crate::symbol_resolver::specialized_function_params(
            &selected,
            &family.kinds,
            type_arguments,
        );
        let value_parameters = parameters.get(selected.context_count..)?.to_vec();
        Some(MappedTopLevelSelection {
            selected,
            value_parameters,
            kinds: family.kinds.clone(),
            slots: family.slots.clone(),
        })
    }

    /// A postponed argument can keep applicability undecided; a single visible declaration still
    /// supplies the provisional shape, and final selection runs once the argument is materialized.
    fn unique_visible_candidate(
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        family: &MappingFamily,
        type_arguments: &[Ty],
    ) -> Option<MappedTopLevelSelection> {
        let mut visible = family.candidates.iter().filter(|candidate| {
            candidate.kind == crate::libraries::FnKind::TopLevel
                && resolver.non_member_callable_accessible(candidate)
        });
        let selected = visible.next()?.clone();
        if visible.next().is_some() {
            return None;
        }
        let parameters = crate::symbol_resolver::specialized_function_params(
            &selected,
            &family.kinds,
            type_arguments,
        );
        let value_parameters = parameters
            .get(selected.context_count..)
            .unwrap_or_default()
            .to_vec();
        Some(MappedTopLevelSelection {
            selected,
            value_parameters,
            kinds: family.kinds.clone(),
            slots: family.slots.clone(),
        })
    }
}
