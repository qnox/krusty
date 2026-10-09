//! Source identities of a decoded declaration's parameters.

use crate::fir::ResolvedParameterIdentity;
use crate::libraries::CallSig;
use crate::metadata::semantic::KotlinFunction;
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

/// Derive and validate the parameter identities of a decoded top-level function.
pub(crate) fn function_parameter_identities(
    function: &KotlinFunction,
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
    let receiver = usize::from(function.receiver.is_some());
    let physical = CallSig {
        parameter_identities: arguments.clone(),
        ..CallSig::default()
    }
    .physical_parameter_identities(
        function.params.len() + receiver,
        function.context_count,
        function
            .receiver
            .is_some()
            .then_some(function.context_count),
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
