//! The call shape a provider normalizes for a source callable: how it is built, what it says about
//! each parameter, and its transformations.

use super::{CallSig, ParamFlags};
use crate::types::{InlineParameterModifier, Ty};

impl CallSig {
    /// The call shape after parameters supplied outside the source argument list have been removed.
    pub fn suffix(&self, start: usize) -> Self {
        let start = start.min(self.param_names.len());
        CallSig {
            only_input_type_formals: self.only_input_type_formals.clone(),
            reified_type_parameter_ordinals: self.reified_type_parameter_ordinals.clone(),
            param_names: self.param_names[start..].to_vec(),
            parameter_identities: self
                .parameter_identities
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            param_defaults: self.param_defaults.get(start..).unwrap_or_default().into(),
            exact_params: self.exact_params.get(start..).unwrap_or_default().into(),
            no_infer_params: self.no_infer_params.get(start..).unwrap_or_default().into(),
            implicit_integer_coercion: self
                .implicit_integer_coercion
                .get(start..)
                .unwrap_or_default()
                .into(),
            lambda_param_types: self
                .lambda_param_types
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            lambda_receivers: self
                .lambda_receivers
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            lambda_receiver_params: self
                .lambda_receiver_params
                .get(start..)
                .unwrap_or_default()
                .into(),
            lambda_context_counts: self
                .lambda_context_counts
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            inline_modifiers: self
                .inline_modifiers
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            platform_nullable_params: self
                .platform_nullable_params
                .get(start..)
                .unwrap_or_default()
                .into(),
            result_enhancement: self.result_enhancement,
            required: self.required.saturating_sub(start),
            vararg: self.vararg,
            vararg_index: self.vararg_index.and_then(|index| index.checked_sub(start)),
        }
    }

    /// The source call shape projected to the selected declaration parameters, in their supplied
    /// order. Representation-only parameters are omitted before argument mapping reaches this API.
    pub fn select_parameters(&self, parameters: &[usize]) -> Self {
        fn selected<T: Clone>(values: &[T], parameters: &[usize]) -> Vec<T> {
            parameters
                .iter()
                .filter_map(|&parameter| values.get(parameter).cloned())
                .collect()
        }

        let param_defaults = selected(&self.param_defaults, parameters);
        let vararg_index = self
            .vararg_index
            .and_then(|vararg| parameters.iter().position(|&parameter| parameter == vararg));
        Self {
            only_input_type_formals: self.only_input_type_formals.clone(),
            reified_type_parameter_ordinals: self.reified_type_parameter_ordinals.clone(),
            param_names: selected(&self.param_names, parameters),
            parameter_identities: selected(&self.parameter_identities, parameters),
            exact_params: selected(&self.exact_params, parameters).into(),
            no_infer_params: selected(&self.no_infer_params, parameters).into(),
            implicit_integer_coercion: selected(&self.implicit_integer_coercion, parameters).into(),
            lambda_param_types: selected(&self.lambda_param_types, parameters),
            lambda_receivers: selected(&self.lambda_receivers, parameters),
            lambda_receiver_params: selected(&self.lambda_receiver_params, parameters).into(),
            lambda_context_counts: selected(&self.lambda_context_counts, parameters),
            inline_modifiers: selected(&self.inline_modifiers, parameters),
            platform_nullable_params: selected(&self.platform_nullable_params, parameters).into(),
            result_enhancement: self.result_enhancement,
            required: crate::libraries::required_arity(parameters.len(), &param_defaults),
            param_defaults: param_defaults.into(),
            vararg: vararg_index.is_some(),
            vararg_index,
        }
    }
}

impl CallSig {
    pub fn parameter_contributes_to_inference(&self, index: usize) -> bool {
        !self.no_infer_params.get(index).copied().unwrap_or(false)
    }

    /// Apply declaration-owned parameter constraints after generic substitution. Every overload
    /// path uses this predicate so `@Exact` cannot drift between candidate families.
    pub fn parameter_admits(&self, index: usize, expected: Ty, actual: Ty) -> bool {
        !self.exact_params.get(index).copied().unwrap_or(false) || expected == actual
    }

    pub fn has_param_names(&self) -> bool {
        !self.param_names.is_empty()
    }

    pub fn has_known_required_param(&self, mut range: std::ops::Range<usize>) -> bool {
        // A vararg slot is never required: it may always be empty (matching the slot mapper's
        // `vararg == Some(parameter_index)` rule in `map_call_args`), even though metadata never
        // sets its `declares_default_value` flag.
        !self.param_defaults.is_empty()
            && range.any(|i| !self.param_has_default(i) && self.vararg_index != Some(i))
    }

    pub fn param_has_default(&self, idx: usize) -> bool {
        self.param_defaults.get(idx).copied().unwrap_or(false)
    }

    pub fn can_map_omitted_args(&self, param_count: usize) -> bool {
        (self.required < param_count || self.vararg_index.is_some()) && self.has_param_names()
    }

    pub fn source(
        param_names: Vec<String>,
        param_defaults: impl Into<ParamFlags>,
        lambda_param_types: Vec<Vec<Ty>>,
        lambda_recv: impl Into<ParamFlags>,
        lambda_context_counts: Vec<usize>,
        required: usize,
        vararg_index: Option<usize>,
    ) -> Self {
        let lambda_recv: ParamFlags = lambda_recv.into();
        let lambda_receivers = lambda_recv
            .iter()
            .enumerate()
            .map(|(i, has_recv)| {
                if *has_recv {
                    lambda_param_types
                        .get(i)
                        .and_then(|parameters| {
                            parameters
                                .get(lambda_context_counts.get(i).copied().unwrap_or_default())
                        })
                        .copied()
                } else {
                    None
                }
            })
            .collect();
        CallSig {
            parameter_identities: param_names
                .iter()
                .enumerate()
                .map(|(ordinal, name)| {
                    if name.is_empty() {
                        crate::fir::ResolvedParameterIdentity::Unnamed {
                            ordinal: ordinal as u32,
                        }
                    } else {
                        crate::fir::ResolvedParameterIdentity::Source(name.as_str().into())
                    }
                })
                .collect(),
            param_names,
            param_defaults: param_defaults.into(),
            lambda_param_types,
            lambda_receivers,
            lambda_receiver_params: lambda_recv,
            lambda_context_counts,
            required,
            vararg: vararg_index.is_some(),
            vararg_index,
            ..Default::default()
        }
    }

    pub fn metadata_member(
        param_count: usize,
        names: Vec<String>,
        defaults: Vec<bool>,
        vararg_index: Option<usize>,
    ) -> Self {
        CallSig::metadata_base(param_count, names, defaults, vararg_index)
    }

    pub fn metadata_plain(param_count: usize) -> Self {
        CallSig::metadata_base(param_count, Vec::new(), Vec::new(), None)
    }

    /// Build the source call shape for a Kotlin function decoded from metadata. `param_count` is
    /// always the source VALUE-parameter count: dispatch and extension receivers are not call
    /// arguments and therefore never appear in this structure.
    pub fn metadata_function(
        param_count: usize,
        names: Vec<String>,
        defaults: Vec<bool>,
        lambda_receivers: Vec<Option<Ty>>,
        lambda_receiver_params: Vec<bool>,
        inline_modifiers: Vec<InlineParameterModifier>,
        vararg_index: Option<usize>,
    ) -> Self {
        let mut sig = CallSig::metadata_base(param_count, names, defaults, vararg_index);
        sig.set_lambda_receiver_shape(param_count, lambda_receivers, lambda_receiver_params);
        sig.inline_modifiers = vec_for_arity(inline_modifiers, param_count);
        sig
    }

    /// Record which value parameters are RECEIVER function types (`Recv.() -> R`, from
    /// `@Metadata`'s `@ExtensionFunctionType`): the receiver's type per parameter
    /// (`lambda_receivers`) and the plain flag (`lambda_receiver_params`). Each list is kept only
    /// when it aligns with `param_count`, so consumers can index positionally without
    /// re-validating arity.
    fn set_lambda_receiver_shape(
        &mut self,
        param_count: usize,
        lambda_receivers: Vec<Option<Ty>>,
        lambda_receiver_params: Vec<bool>,
    ) {
        self.lambda_receivers = vec_for_arity(lambda_receivers, param_count);
        self.lambda_receiver_params = vec_for_arity(lambda_receiver_params, param_count).into();
    }

    fn metadata_base(
        param_count: usize,
        names: Vec<String>,
        defaults: Vec<bool>,
        vararg_index: Option<usize>,
    ) -> Self {
        // Legacy context receivers are unnamed but precede ordinary value parameters. Retain their
        // empty slots so the named suffix stays positionally aligned; consumers already hide the
        // leading context slots from explicit argument mapping.
        let names = vec_for_arity(names, param_count);
        let defaults = vec_for_arity(defaults, param_count);
        let defaults = if defaults.iter().any(|d| *d) {
            defaults
        } else {
            Vec::new()
        };
        CallSig {
            required: required_arity(param_count, &defaults),
            parameter_identities: (0..param_count)
                .map(
                    |ordinal| match names.get(ordinal).filter(|name| !name.is_empty()) {
                        Some(name) => {
                            crate::fir::ResolvedParameterIdentity::Source(name.as_str().into())
                        }
                        None => crate::fir::ResolvedParameterIdentity::Unnamed {
                            ordinal: ordinal as u32,
                        },
                    },
                )
                .collect(),
            param_names: names,
            param_defaults: defaults.into(),
            vararg: vararg_index.is_some(),
            vararg_index,
            ..Default::default()
        }
    }

    /// Insert a typed extension-receiver identity into this provider-published logical list.
    pub fn physical_parameter_identities(
        &self,
        physical_count: usize,
        context_count: usize,
        extension_receiver_position: Option<usize>,
    ) -> Option<Box<[crate::fir::ResolvedParameterIdentity]>> {
        let logical_count =
            physical_count.checked_sub(usize::from(extension_receiver_position.is_some()))?;
        if self.parameter_identities.len() != logical_count || context_count > logical_count {
            return None;
        }
        let identities_are_typed = self.parameter_identities.iter().enumerate().all(
            |(ordinal, identity)| {
                let context = matches!(
                    identity,
                    crate::fir::ResolvedParameterIdentity::ContextValue { .. }
                        | crate::fir::ResolvedParameterIdentity::AnonymousContextParameter { .. }
                        | crate::fir::ResolvedParameterIdentity::LegacyContextReceiver { .. }
                );
                context == (ordinal < context_count)
            },
        );
        if !identities_are_typed {
            return None;
        }
        let mut identities = self.parameter_identities.clone();
        if let Some(position) = extension_receiver_position {
            if position > identities.len() {
                return None;
            }
            identities.insert(
                position,
                crate::fir::ResolvedParameterIdentity::ExtensionReceiver,
            );
        }
        Some(identities.into_boxed_slice())
    }
}

pub fn required_arity(param_count: usize, defaults: &[bool]) -> usize {
    if defaults.is_empty() {
        param_count
    } else {
        defaults.iter().filter(|d| !**d).count()
    }
}

fn vec_for_arity<T>(items: Vec<T>, param_count: usize) -> Vec<T> {
    if items.len() == param_count {
        items
    } else {
        Vec::new()
    }
}
