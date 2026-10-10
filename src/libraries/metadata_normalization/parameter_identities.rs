//! Source identities of a decoded declaration's parameters.

use crate::fir::ResolvedParameterIdentity;
use crate::libraries::CallSig;
use crate::metadata::semantic::{KotlinConstructor, KotlinFunction, KotlinProperty};
use crate::types::ContextParameterKind;

/// A decoded declaration whose parameter list does not describe one consistent source shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InconsistentParameters {
    detail: String,
}

impl std::fmt::Display for InconsistentParameters {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

/// The source identities of a top-level function's parameters, validated once against its shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FunctionParameterIdentities {
    /// One per call argument: the leading context parameters, each by its declared role, then the
    /// value parameters by source name. The extension receiver is not a call argument.
    pub(crate) arguments: Vec<ResolvedParameterIdentity>,
    /// One per physical parameter, as a realization publishes them: the context parameters, the
    /// extension receiver, then the value parameters.
    pub(crate) physical: Box<[ResolvedParameterIdentity]>,
}

/// Derive and validate the parameter identities of a decoded function: a top-level function or
/// extension, or a member or member extension, whose extension receiver is a parameter.
pub(crate) fn function_parameter_identities(
    function: &KotlinFunction,
) -> Result<FunctionParameterIdentities, InconsistentParameters> {
    identities_of_function(function, function.receiver.is_some())
}

/// Derive and validate the parameter identities of a function named through a classifier with no
/// value operand: a `companion { … }` block member, or a companion extension, whose written
/// receiver names that classifier and is not a parameter.
pub(crate) fn associated_function_parameter_identities(
    function: &KotlinFunction,
) -> Result<FunctionParameterIdentities, InconsistentParameters> {
    identities_of_function(function, false)
}

fn identities_of_function(
    function: &KotlinFunction,
    has_receiver_parameter: bool,
) -> Result<FunctionParameterIdentities, InconsistentParameters> {
    let inconsistent = |detail: String| InconsistentParameters { detail };
    if function.param_names.len() != function.params.len() {
        return Err(inconsistent(format!(
            "function {} has {} parameters but {} parameter names",
            function.name,
            function.params.len(),
            function.param_names.len()
        )));
    }
    if function.context_kinds.len() != function.context_count
        || function.context_count > function.params.len()
    {
        return Err(inconsistent(format!(
            "function {} declares {} context parameters with {} roles among {} parameters",
            function.name,
            function.context_count,
            function.context_kinds.len(),
            function.params.len()
        )));
    }
    let roles = function
        .context_kinds
        .iter()
        .copied()
        .chain(std::iter::repeat(ContextParameterKind::None));
    let arguments: Vec<_> = function
        .param_names
        .iter()
        .zip(roles)
        .enumerate()
        .map(|(ordinal, (name, role))| {
            ResolvedParameterIdentity::declared(
                u32::try_from(ordinal).expect("a declaration's parameter ordinal fits u32"),
                name,
                role,
            )
        })
        .collect();
    let receiver = usize::from(has_receiver_parameter);
    let physical = CallSig {
        parameter_identities: arguments.clone(),
        ..CallSig::default()
    }
    .physical_parameter_identities(
        function.params.len() + receiver,
        function.context_count,
        has_receiver_parameter.then_some(function.context_count),
    )
    .ok_or_else(|| {
        inconsistent(format!(
            "function {} has parameter identities that do not match its {} parameters",
            function.name,
            function.params.len() + receiver
        ))
    })?;
    Ok(FunctionParameterIdentities {
        arguments,
        physical,
    })
}

/// The source identities of a top-level property's context parameters and of its accessors'
/// parameters, validated once against its shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PropertyParameterIdentities {
    /// One per context parameter, each by its declared role.
    pub(crate) contexts: Vec<ResolvedParameterIdentity>,
    /// One per physical getter parameter: the context parameters, then the extension receiver.
    pub(crate) getter: Box<[ResolvedParameterIdentity]>,
    /// One per physical setter parameter of a `var`: the getter's, then the assigned value.
    pub(crate) setter: Option<Box<[ResolvedParameterIdentity]>>,
}

/// Derive and validate the parameter identities of a decoded property whose extension receiver,
/// if it declares one, is an accessor parameter.
pub(crate) fn property_parameter_identities(
    property: &KotlinProperty,
) -> Result<PropertyParameterIdentities, InconsistentParameters> {
    identities_of_property(property, property.receiver.is_some())
}

/// Derive and validate the parameter identities of a property named through a classifier with no
/// value operand; as for [`associated_function_parameter_identities`], its written receiver is
/// not an accessor parameter.
pub(crate) fn associated_property_parameter_identities(
    property: &KotlinProperty,
) -> Result<PropertyParameterIdentities, InconsistentParameters> {
    identities_of_property(property, false)
}

fn identities_of_property(
    property: &KotlinProperty,
    has_receiver_parameter: bool,
) -> Result<PropertyParameterIdentities, InconsistentParameters> {
    let count = property.context_params.len();
    if property.context_count != count
        || property.context_kinds.len() != count
        || property.context_param_names.len() != count
    {
        return Err(InconsistentParameters {
            detail: format!(
                "property {} declares {} context parameters with {} types, {} roles and {} names",
                property.name,
                property.context_count,
                count,
                property.context_kinds.len(),
                property.context_param_names.len()
            ),
        });
    }
    if property.context_kinds.contains(&ContextParameterKind::None) {
        return Err(InconsistentParameters {
            detail: format!(
                "property {} has a context parameter without a context role",
                property.name
            ),
        });
    }
    let contexts: Vec<_> = property
        .context_param_names
        .iter()
        .zip(&property.context_kinds)
        .enumerate()
        .map(|(ordinal, (name, role))| {
            ResolvedParameterIdentity::declared(
                u32::try_from(ordinal).expect("a declaration's parameter ordinal fits u32"),
                name,
                *role,
            )
        })
        .collect();
    let getter: Box<[_]> = contexts
        .iter()
        .cloned()
        .chain(has_receiver_parameter.then_some(ResolvedParameterIdentity::ExtensionReceiver))
        .collect();
    let setter = property.is_var.then(|| {
        let value = property
            .setter_parameter_name
            .as_deref()
            .map_or(ResolvedParameterIdentity::PropertySetterValue, |name| {
                ResolvedParameterIdentity::Source(name.into())
            });
        getter.iter().cloned().chain([value]).collect()
    });
    Ok(PropertyParameterIdentities {
        contexts,
        getter,
        setter,
    })
}

/// Derive and validate the parameter identities of a decoded constructor: one per value
/// parameter, by source name.
pub(crate) fn constructor_parameter_identities(
    constructor: &KotlinConstructor,
) -> Result<Box<[ResolvedParameterIdentity]>, InconsistentParameters> {
    if constructor.param_names.len() != constructor.params.len() {
        return Err(InconsistentParameters {
            detail: format!(
                "constructor has {} parameters but {} parameter names",
                constructor.params.len(),
                constructor.param_names.len()
            ),
        });
    }
    Ok(constructor
        .param_names
        .iter()
        .enumerate()
        .map(|(ordinal, name)| {
            ResolvedParameterIdentity::declared(
                u32::try_from(ordinal).expect("a declaration's parameter ordinal fits u32"),
                name,
                ContextParameterKind::None,
            )
        })
        .collect())
}
