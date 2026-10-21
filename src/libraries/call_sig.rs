//! Provider-normalized source call shapes, and transformations over them.

use std::sync::{Arc, LazyLock};

use super::{InlineParameterModifier, ParamFlags, ResultEnhancement};
use crate::types::Ty;

/// The source-level call shape of one overload — the call-site facts the CHECKER needs that the erased
/// emit `descriptor` drops. Parallel to the LOGICAL parameter list (the receiver is NOT included, even
/// for an extension whose `callable.params` prepends it). Empty/zero `Default` means "not provided by
/// this source"; the federated consumer falls back as it did before the consolidation.
#[derive(Clone, Default, Debug)]
pub struct CallShape {
    /// Function type parameters annotated with Kotlin's internal `@OnlyInputTypes`.
    ///
    /// This is an inference policy, not parameter nullability: the inferred value of each named
    /// formal must be one of the proper types contributed by a call input (receiver, value
    /// argument, or expected result). Keeping the declaration-owned names here lets every provider
    /// expose the same rule without teaching resolution about stdlib function names.
    pub only_input_type_formals: Vec<String>,
    /// Declaration type-parameter ordinals carrying Kotlin's `reified` modifier. This is provider-
    /// normalized callable capability; call lowering uses it to distinguish a runtime reified
    /// argument from an ordinary substitution without inspecting source names.
    pub reified_type_parameter_ordinals: Vec<u32>,
    /// Parameter names, parallel to the logical params — maps named arguments (`f(x = 1)`) to positions.
    pub param_names: Vec<String>,
    /// Provider-published semantic identities parallel to the logical parameter list. Context
    /// roles are typed at the declaration boundary; consumers never rediscover them from `_`, an
    /// empty metadata name, or another spelling convention.
    pub parameter_identities: Vec<crate::fir::ResolvedParameterIdentity>,
    /// Per logical param: whether it has a default value (so it may be omitted). Parallel to the params.
    pub param_defaults: ParamFlags,
    /// Per logical parameter: the declared type is annotated with Kotlin's internal `@Exact`, so
    /// applicability requires equality after generic substitution rather than ordinary subtyping.
    pub exact_params: ParamFlags,
    /// Per logical parameter: the declared type carries Kotlin's internal `@NoInfer`, so the
    /// argument is checked for applicability after inference but contributes no type-variable
    /// constraint itself.
    pub no_infer_params: ParamFlags,
    /// Per logical parameter, the resolved declaration carries
    /// `kotlin.internal.ImplicitIntegerCoercion`.
    pub implicit_integer_coercion: ParamFlags,
    /// Per logical param: if it is a function type `(A, B) -> R`, its inner param types `[A, B]` (to type
    /// a lambda argument's `it`/params); otherwise empty. Parallel to the params.
    pub lambda_param_types: Vec<Vec<Ty>>,
    /// Per logical param: `Some(receiver)` when the parameter is a receiver function type
    /// `Receiver.(...) -> R`. The checker binds that receiver as lambda `this` while using
    /// `lambda_param_types` for the receiver/value parameters recovered from the generic signature.
    pub lambda_receivers: Vec<Option<Ty>>,
    /// Per logical param: whether it is a receiver function type, even when metadata cannot name a
    /// concrete receiver class because the receiver is a type parameter (`T.() -> R`).
    pub lambda_receiver_params: ParamFlags,
    /// Leading context receiver count for each function-typed parameter.
    pub lambda_context_counts: Vec<usize>,
    /// Per logical param: its `crossinline`/`noinline` modifier ([`InlineParameterModifier`]).
    /// Parallel to the params; empty when the source publishes none.
    pub inline_modifiers: Vec<InlineParameterModifier>,
    /// Per logical Java parameter, whether nullable arguments are accepted.
    pub platform_nullable_params: ParamFlags,
    /// Java signature enhancement of the declared result's nullability.
    pub result_enhancement: ResultEnhancement,
    /// Minimum arguments a caller must supply (params beyond this have defaults). 0 by default.
    pub required: usize,
    /// True if a logical param is `vararg` (callers pack values into its array).
    pub vararg: bool,
    pub vararg_index: Option<usize>,
}

/// One overload's [`CallShape`], shared by every record that carries it. A declaration's shape is
/// built once and travels unchanged through its classifier records, candidate copies and selected
/// callables, so those copies share one allocation; a write goes through [`Arc::make_mut`] and
/// copies the shape only when another record still shares it. Every empty shape shares one
/// allocation.
#[derive(Clone)]
pub struct CallSig(Arc<CallShape>);

static EMPTY: LazyLock<CallSig> = LazyLock::new(|| CallSig(Arc::new(CallShape::default())));

impl Default for CallSig {
    fn default() -> Self {
        EMPTY.clone()
    }
}

impl From<CallShape> for CallSig {
    fn from(shape: CallShape) -> Self {
        CallSig(Arc::new(shape))
    }
}

impl std::ops::Deref for CallSig {
    type Target = CallShape;

    fn deref(&self) -> &CallShape {
        &self.0
    }
}

impl std::ops::DerefMut for CallSig {
    fn deref_mut(&mut self) -> &mut CallShape {
        Arc::make_mut(&mut self.0)
    }
}

impl std::fmt::Debug for CallSig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl CallSig {
    /// The call shape after parameters supplied outside the source argument list have been removed.
    pub fn suffix(&self, start: usize) -> Self {
        let start = start.min(self.param_names.len());
        CallSig::from(CallShape {
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
        })
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
        CallSig::from(CallShape {
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
        })
    }
}

// Size budgets for the records every classifier holds per method. The call shape lives behind a
// shared handle so a record holds one pointer for it; growing a budget is a deliberate decision.
const _: () = {
    use std::mem::size_of;
    assert!(size_of::<CallSig>() == size_of::<usize>());
    assert!(size_of::<super::FunctionInfo>() <= 856);
    assert!(size_of::<super::LibraryMember>() <= 592);
    assert!(size_of::<super::LibraryType>() <= 1416);
};
