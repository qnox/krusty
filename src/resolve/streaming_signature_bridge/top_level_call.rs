//! The outcome of selecting a top-level call spelling: a callable, a value, a classifier's
//! `invoke`, or a construction together with the typealias that named the constructed classifier.

pub(super) enum SelectedTopLevelCall {
    Ambiguous,
    Callable {
        callable: Box<crate::libraries::LibraryCallable>,
        source: Option<(u32, u32)>,
        declaration: Option<crate::fir::DeclarationId>,
        parameter_by_argument: Box<[Option<u32>]>,
    },
    Value(Box<crate::libraries::PropertyInfo>),
    /// A classifier whose constructors do not apply: its associated `operator fun invoke`, then the
    /// `invoke` convention on the value it denotes (an object singleton or companion), if any.
    ClassifierInvoke(crate::types::TypeName, Option<crate::types::Ty>),
    Constructor {
        declaration: Box<crate::symbol_resolver::SelectedConstructorDeclaration>,
        alias: SelectedAlias,
        /// Exact source-argument to declaration-parameter mapping retained by the winning
        /// constructor candidate. Publication must not try to map the call again.
        parameter_by_argument: Box<[Option<u32>]>,
    },
    /// A fun-interface name applied to one function value (`I { … }`): no constructor, the result
    /// is the interface. Both constructions carry the alias the winning scope rung named, if any.
    SamConstructor(crate::types::TypeName, SelectedAlias),
}

pub(super) type SelectedAlias = Option<Box<crate::libraries::AliasExpansion>>;

impl super::ProductionSignatureSemantics<'_> {
    pub(super) fn constructor_argument_mapping(
        candidate: &crate::libraries::LibraryMember,
        source_indices: &[usize],
        names: &[Option<String>],
        trailing_lambda: bool,
    ) -> Option<(Vec<Option<usize>>, Vec<usize>)> {
        let slots = crate::libraries::map_call_args(
            source_indices,
            Some(names),
            &candidate.call_sig.param_names,
            candidate.params.len(),
            candidate.call_sig.required,
            &candidate.call_sig.param_defaults,
            candidate.call_sig.vararg_index,
            trailing_lambda,
        )
        .ok()?;
        let parameter_by_argument = source_indices
            .iter()
            .map(|source| {
                slots
                    .iter()
                    .position(|slot| slot == &Some(*source))
                    .or(candidate.call_sig.vararg_index)
            })
            .collect::<Option<Vec<_>>>()?;
        Some((slots, parameter_by_argument))
    }

    /// Select a constructor from source-ordered positional arguments, then retain the argument map
    /// of that exact declaration. Named/default/trailing calls first use their common unambiguous
    /// mapping; positional vararg overloads remain candidate-specific until overload selection.
    pub(super) fn selected_constructor_arguments(
        resolver: &crate::symbol_resolver::SymbolResolver<'_>,
        classifier: crate::types::TypeName,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        trailing_lambda: bool,
        type_arguments: &[crate::types::Ty],
    ) -> Option<(
        crate::symbol_resolver::SelectedConstructorDeclaration,
        MappedConstructorArguments,
    )> {
        let positional =
            !trailing_lambda && arguments.iter().all(|argument| argument.name.is_none());
        let selection_arguments = if positional {
            MappedConstructorArguments::mapped(
                arguments.iter().map(Self::call_argument_kind).collect(),
                arguments.iter().map(|argument| argument.ty.get()).collect(),
                (0..arguments.len())
                    .map(|parameter| {
                        Some(
                            u32::try_from(parameter)
                                .expect("a constructor parameter ordinal must fit u32"),
                        )
                    })
                    .collect(),
            )
        } else {
            Self::mapped_constructor_arguments(resolver, classifier, arguments, trailing_lambda)?
        };
        let selected = resolver.select_constructor_declaration_with_type_arguments(
            classifier,
            &selection_arguments.kinds,
            type_arguments,
        )?;
        let source_indices = (0..arguments.len()).collect::<Vec<_>>();
        let names = arguments
            .iter()
            .map(|argument| argument.name.map(str::to_owned))
            .collect::<Vec<_>>();
        let (_, parameter_by_argument) = Self::constructor_argument_mapping(
            &selected.declaration,
            &source_indices,
            &names,
            trailing_lambda,
        )?;
        let parameter_by_argument = parameter_by_argument
            .into_iter()
            .map(|parameter| {
                Some(
                    u32::try_from(parameter).expect("a constructor parameter ordinal must fit u32"),
                )
            })
            .collect();
        Some((
            selected,
            MappedConstructorArguments::mapped(
                selection_arguments.kinds,
                selection_arguments.types,
                parameter_by_argument,
            ),
        ))
    }
}

/// A top-level declaration that overload selection accepted under its own argument mapping, when
/// the family has no single mapping (its members name or omit different parameters).
pub(super) struct IndividuallyApplicableCall {
    pub(super) omitted_defaults: usize,
    pub(super) declaration: crate::libraries::FunctionInfo,
    pub(super) callable: crate::libraries::LibraryCallable,
    pub(super) argument_types: Vec<crate::types::Ty>,
}

impl super::ProductionSignatureSemantics<'_> {
    /// Kotlin ranks the individually applicable declarations as at any call: the most specific
    /// parameters the supplied arguments map to, compared in argument order, win; a parameter left
    /// to its default takes no part. Only declarations whose compared parameters are identical are
    /// separated by the defaults they omit, the fewest winning. `None` is an ambiguity.
    pub(super) fn most_specific_individual_call(
        source: &dyn crate::symbol_source::SymbolSource,
        arguments: &[crate::fir::ResolvedSigCallArgument<'_>],
        trailing_lambda: bool,
        applicable: Vec<IndividuallyApplicableCall>,
    ) -> Option<(IndividuallyApplicableCall, Box<[Option<u32>]>)> {
        let mut ranked = applicable
            .into_iter()
            .map(|call| {
                let parameter_by_argument = Self::selected_argument_parameters(
                    &call.declaration,
                    arguments,
                    trailing_lambda,
                );
                let parameters = call.declaration.semantic_params();
                let shape = arguments
                    .iter()
                    .zip(&parameter_by_argument)
                    .map(|(argument, parameter)| {
                        let index = parameter.and_then(|index| usize::try_from(index).ok())?;
                        let parameter = *parameters.get(index)?;
                        // A plain vararg element compares against the element type, a spread
                        // against the array.
                        Some(
                            if call.declaration.call_sig.vararg_index == Some(index)
                                && !argument.spread
                            {
                                parameter.array_read_elem().unwrap_or(parameter)
                            } else {
                                parameter
                            },
                        )
                    })
                    .collect::<Option<Vec<_>>>()
                    .expect("a selected call maps every source argument to a parameter");
                (call, parameter_by_argument, shape)
            })
            .collect::<Vec<_>>();
        let fewer_defaults_same_shape = ranked
            .iter()
            .enumerate()
            .filter(|(_, (call, _, shape))| {
                ranked.iter().any(|(other, _, other_shape)| {
                    other_shape == shape && other.omitted_defaults < call.omitted_defaults
                })
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in fewer_defaults_same_shape.into_iter().rev() {
            ranked.remove(index);
        }
        let index = if ranked.len() == 1 {
            0
        } else {
            let shapes = ranked
                .iter()
                .map(|(_, _, shape)| shape.clone())
                .collect::<Vec<_>>();
            let kinds = arguments
                .iter()
                .map(Self::call_argument_kind)
                .collect::<Vec<_>>();
            match crate::symbol_resolver::most_specific_parameter_shape_index(
                source, &shapes, &kinds,
            ) {
                crate::symbol_resolver::CandidateSelection::Selected(index) => index,
                crate::symbol_resolver::CandidateSelection::None
                | crate::symbol_resolver::CandidateSelection::Ambiguous => return None,
            }
        };
        let (call, parameter_by_argument, _) = ranked.swap_remove(index);
        Some((call, parameter_by_argument))
    }
}

/// Constructor arguments in the exact parameter order supplied to overload selection, together
/// with the source-slot mapping that produced that order. Publication consumes this retained map;
/// it never reruns argument mapping against the selected declaration.
pub(super) struct MappedConstructorArguments {
    pub(super) kinds: Vec<crate::symbol_resolver::CallArgKind>,
    pub(super) types: Vec<crate::types::Ty>,
    parameter_by_argument: Box<[Option<u32>]>,
}

impl MappedConstructorArguments {
    pub(super) fn mapped(
        kinds: Vec<crate::symbol_resolver::CallArgKind>,
        types: Vec<crate::types::Ty>,
        parameter_by_argument: Box<[Option<u32>]>,
    ) -> Self {
        Self {
            kinds,
            types,
            parameter_by_argument,
        }
    }

    pub(super) fn parameter_by_argument(&self) -> Box<[Option<u32>]> {
        self.parameter_by_argument.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    #[test]
    fn selected_constructor_consumes_its_retained_argument_mapping() {
        let mapped = MappedConstructorArguments::mapped(
            vec![
                crate::symbol_resolver::CallArgKind::Typed(Ty::String),
                crate::symbol_resolver::CallArgKind::Typed(Ty::Int),
            ],
            vec![Ty::String, Ty::Int],
            vec![Some(1), Some(0)].into_boxed_slice(),
        );

        assert_eq!(
            mapped.parameter_by_argument(),
            vec![Some(1), Some(0)].into_boxed_slice(),
        );
    }
}
