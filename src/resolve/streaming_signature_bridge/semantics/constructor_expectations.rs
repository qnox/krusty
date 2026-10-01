//! Postponed-argument expectations contributed by classifier constructors.
//!
//! A constructor is an ordinary callable candidate: once a scope-tower rung has bound the
//! classifier (a bare classifier call, or an inner class reached through a receiver), its
//! constructors supply parameter shapes for postponed arguments exactly like functions do. At a
//! receiver rung the inner classifier's constructors belong to the receiver's member level and
//! form one candidate family with its same-named member functions.

use super::*;

/// One call on a receiver member level (`outer.Inner(args)`, or a bare `Inner(args)` on an
/// implicit receiver) whose postponed arguments need expected types.
#[derive(Clone, Copy)]
pub(in crate::resolve::streaming_signature_bridge) struct ReceiverLevelCall<'call, 'source> {
    pub(in crate::resolve::streaming_signature_bridge) scope: crate::fir::SignatureScope,
    pub(in crate::resolve::streaming_signature_bridge) spelling: &'call str,
    pub(in crate::resolve::streaming_signature_bridge) arguments:
        &'call [crate::fir::SigCallArgumentProbe<'source>],
    pub(in crate::resolve::streaming_signature_bridge) type_arguments: &'call [Ty],
    pub(in crate::resolve::streaming_signature_bridge) trailing_lambda: bool,
}

/// Parameter shapes in declaration order, with the source argument each parameter slot receives.
type MappedParameters = (Vec<Ty>, Vec<Option<usize>>);

impl ProductionSignatureSemantics<'_> {
    /// Postponed-argument shapes from `receiver`'s member level, with the source argument each
    /// parameter slot receives. The level's candidates, same-named inner constructors included,
    /// are one family; an ambiguous family yields only the shapes its candidates share, never one
    /// member picked in preference to another. `None` when nothing on the level applies, so the
    /// scope tower continues outward.
    pub(in crate::resolve::streaming_signature_bridge) fn receiver_member_level_expectations(
        &self,
        receiver: Ty,
        call: ReceiverLevelCall<'_, '_>,
    ) -> Result<Option<MappedParameters>, crate::fir::DiagnosticId> {
        let ReceiverLevelCall {
            scope,
            spelling,
            arguments,
            type_arguments,
            trailing_lambda,
        } = call;
        let family = self.with_resolver(scope, |resolver| {
            Some(
                self.receiver_member_level(scope, resolver, receiver, spelling)
                    .map(|callables| {
                        self.receiver_family_postponed_parameters(
                            resolver,
                            callables,
                            super::super::postponed_calls::PostponedReceiverCall {
                                scope,
                                receiver,
                                spelling,
                                arguments,
                                type_arguments,
                                trailing_lambda,
                            },
                        )
                    }),
            )
        })??;
        crate::trace_compiler!(
            "signature",
            "member level expectation {spelling} receiver={receiver:?} family={family:?}",
        );
        Ok(family)
    }

    /// Contextual shapes contributed by the constructors of one already-resolved classifier,
    /// used where a bare classifier call is the selected scope-tower rung.
    ///
    /// The constructors are one candidate family. Each declaration's own parameter names,
    /// defaults, and vararg decide whether it can consume the source arguments. A positional call
    /// then selects among the constructors with the constructor overload engine; when selection
    /// cannot decide before the postponed arguments are materialized, or the call names or trails
    /// its arguments, a postponed argument receives only the shape every mapped constructor
    /// shares. No constructor is chosen by scanning for a single structural match.
    pub(super) fn constructor_call_argument_expectations(
        &self,
        scope: crate::fir::SignatureScope,
        internal: crate::types::TypeName,
        arguments: &[crate::fir::SigCallArgumentProbe<'_>],
        type_arguments: &[Ty],
        trailing_lambda: bool,
    ) -> Result<Box<[Option<crate::fir::ResolvedTy>]>, crate::fir::DiagnosticId> {
        let (parameters, slots) = self.with_resolver(scope, |resolver| {
            let source_probes = arguments
                .iter()
                .map(Self::probe_argument_kind)
                .collect::<Vec<_>>();
            let source_indices = (0..arguments.len()).collect::<Vec<_>>();
            let names = arguments
                .iter()
                .map(|argument| match argument {
                    crate::fir::SigCallArgumentProbe::Typed(argument) => {
                        argument.name.map(str::to_owned)
                    }
                    crate::fir::SigCallArgumentProbe::PostponedLambda { name, .. }
                    | crate::fir::SigCallArgumentProbe::PostponedCallableReference {
                        name, ..
                    } => name.map(str::to_owned),
                })
                .collect::<Vec<_>>();
            let call_slots = |constructor: &crate::libraries::LibraryMember| {
                crate::libraries::map_call_args(
                    &source_indices,
                    Some(&names),
                    &constructor.call_sig.param_names,
                    constructor.params.len(),
                    constructor.call_sig.required,
                    &constructor.call_sig.param_defaults,
                    constructor.call_sig.vararg_index,
                    trailing_lambda,
                )
                .ok()
            };
            // One constructor's parameter shapes, specialized from the argument kinds its own slot
            // mapping supplies.
            let parameters = |constructor: &crate::libraries::LibraryMember,
                              slots: &[Option<usize>]| {
                let mapped_probes = slots
                    .iter()
                    .map(|source| {
                        source
                            .and_then(|source| source_probes.get(source).cloned())
                            .unwrap_or(crate::symbol_resolver::CallArgKind::OmittedDefault)
                    })
                    .collect::<Vec<_>>();
                resolver
                    .specialized_constructor_parameter_types(
                        internal,
                        constructor,
                        &mapped_probes,
                        type_arguments,
                    )
                    .into_iter()
                    .map(|parameter| {
                        resolver
                            .functional_expectation(parameter)
                            .unwrap_or(parameter)
                    })
                    .collect::<Vec<_>>()
            };
            let selected = (!trailing_lambda && names.iter().all(Option::is_none))
                .then(|| {
                    resolver.select_constructor_declaration_with_type_arguments(
                        internal,
                        &source_probes,
                        type_arguments,
                    )
                })
                .flatten();
            if let Some(selected) = selected {
                let slots = call_slots(&selected.declaration)?;
                return Some((parameters(&selected.declaration, &slots), slots));
            }
            let classifier = resolver.classifier(internal)?;
            let shapes = classifier
                .constructors
                .iter()
                .filter_map(|constructor| {
                    let slots = call_slots(constructor)?;
                    let mut constructor = constructor.clone();
                    constructor.owner.get_or_insert(internal);
                    let parameters = parameters(&constructor, &slots);
                    Self::source_ordered_parameters(
                        &parameters,
                        &slots,
                        constructor.call_sig.vararg_index,
                        arguments.len(),
                    )
                })
                .collect::<Vec<_>>();
            let parameters = match shapes.as_slice() {
                [shape] => shape.clone(),
                _ => self.common_postponed_parameters(resolver, arguments, shapes)?,
            };
            Some((parameters, source_indices.into_iter().map(Some).collect()))
        })?;
        Ok(Self::postponed_expectations(arguments, &slots, &parameters))
    }

    /// `parameters` of one mapped candidate projected onto source-argument order; the extra
    /// source arguments a vararg absorbs take its parameter. `None` when a source argument has no
    /// parameter.
    fn source_ordered_parameters(
        parameters: &[Ty],
        slots: &[Option<usize>],
        vararg: Option<usize>,
        argument_count: usize,
    ) -> Option<Vec<Ty>> {
        let mut source_parameters = vec![None; argument_count];
        for (parameter, source) in slots.iter().enumerate() {
            if let Some(source) = *source {
                *source_parameters.get_mut(source)? = Some(*parameters.get(parameter)?);
            }
        }
        if let Some(vararg) = vararg {
            let parameter = *parameters.get(vararg)?;
            for source_parameter in &mut source_parameters {
                source_parameter.get_or_insert(parameter);
            }
        }
        source_parameters.into_iter().collect()
    }
}
