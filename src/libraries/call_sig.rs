//! Transformations over provider-normalized source call shapes.

use super::CallSig;

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
            param_defaults: self
                .param_defaults
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            exact_params: self.exact_params.get(start..).unwrap_or_default().to_vec(),
            no_infer_params: self
                .no_infer_params
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
            implicit_integer_coercion: self
                .implicit_integer_coercion
                .get(start..)
                .unwrap_or_default()
                .to_vec(),
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
                .to_vec(),
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
                .to_vec(),
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
            exact_params: selected(&self.exact_params, parameters),
            no_infer_params: selected(&self.no_infer_params, parameters),
            implicit_integer_coercion: selected(&self.implicit_integer_coercion, parameters),
            lambda_param_types: selected(&self.lambda_param_types, parameters),
            lambda_receivers: selected(&self.lambda_receivers, parameters),
            lambda_receiver_params: selected(&self.lambda_receiver_params, parameters),
            lambda_context_counts: selected(&self.lambda_context_counts, parameters),
            inline_modifiers: selected(&self.inline_modifiers, parameters),
            platform_nullable_params: selected(&self.platform_nullable_params, parameters),
            required: crate::libraries::required_arity(parameters.len(), &param_defaults),
            param_defaults,
            vararg: vararg_index.is_some(),
            vararg_index,
        }
    }
}
