use super::{IrExpr, IrFile};
use crate::fir::{ExternalCallableId, ExternalPropertyId};

/// Stable selected dependency identity used by realization diagnostics.
#[derive(Clone, Copy, Debug)]
pub(in crate::jvm) enum ExternalDependencyTarget {
    Callable(ExternalCallableId),
    Property(ExternalPropertyId),
    DispatchClassifier {
        target: ExternalCallableId,
        classifier: crate::types::TypeName,
    },
}

impl From<ExternalCallableId> for ExternalDependencyTarget {
    fn from(target: ExternalCallableId) -> Self {
        Self::Callable(target)
    }
}

impl std::fmt::Display for ExternalDependencyTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Callable(target) => write!(formatter, "callable {}", target.raw()),
            Self::Property(target) => write!(formatter, "property {}", target.raw()),
            Self::DispatchClassifier { target, classifier } => write!(
                formatter,
                "callable {} dispatch classifier {}",
                target.raw(),
                classifier
            ),
        }
    }
}

/// Move checker-selected call facts to the concrete operation when a backend boundary wraps it.
pub(super) fn copy_call_facts(
    ir: &mut IrFile,
    source: crate::ir::ExprId,
    target: crate::ir::ExprId,
) {
    macro_rules! copy {
        ($field:ident) => {
            if let Some(value) = ir.$field.get(&source).cloned() {
                ir.$field.insert(target, value);
            }
        };
    }
    copy!(fir_origins);
    copy!(expr_lines);
    copy!(expr_source_lines);
    copy!(expr_end_lines);
    copy!(logical_types);
    copy!(physical_types);
    copy!(ext_call_source_receiver);
    copy!(dispatch_classes);
    copy!(call_declared_ret);
    copy!(call_declared_params);
    copy!(static_extension_receivers);
    copy!(call_inline_modifiers);
    copy!(reified_call_subst);
    copy!(inline_call_type_arguments);
    if let Some(value) = ir.declaration_argument_boundaries.remove(&source) {
        ir.declaration_argument_boundaries.insert(target, value);
    }
    if let Some(value) = ir.physical_call_parameters.remove(&source) {
        ir.physical_call_parameters.insert(target, value);
    }
    if let Some(value) = ir.suspend_calls.remove(&source) {
        ir.suspend_calls.insert(target, value);
    }
    // A suspension point identifies the selected call operation, not the semantic result wrapper.
    // Move this single-owner fact with the call, as `suspend_calls` does.
    if let Some(value) = ir.suspend_call_overridden_results.remove(&source) {
        ir.suspend_call_overridden_results.insert(target, value);
    }
}

#[derive(Debug)]
pub(in crate::jvm) enum ExternalRealizationError {
    Missing(ExternalDependencyTarget),
    Arguments {
        target: ExternalDependencyTarget,
        detail: String,
    },
}

impl From<ExternalCallableId> for ExternalRealizationError {
    fn from(target: ExternalCallableId) -> Self {
        Self::Missing(ExternalDependencyTarget::Callable(target))
    }
}

impl From<ExternalDependencyTarget> for ExternalRealizationError {
    fn from(target: ExternalDependencyTarget) -> Self {
        Self::Missing(target)
    }
}

impl std::fmt::Display for ExternalRealizationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(target) => write!(formatter, "{target}"),
            Self::Arguments { target, detail } => write!(formatter, "{target}: {detail}"),
        }
    }
}

pub(super) fn materialize_omitted_arguments(
    ir: &mut IrFile,
    parameters: &[crate::types::Ty],
    supplied: Vec<crate::ir::ExprId>,
    omitted: &[u32],
    target: ExternalCallableId,
) -> Result<Vec<crate::ir::ExprId>, ExternalCallableId> {
    if omitted.windows(2).any(|pair| pair[0] >= pair[1])
        || omitted
            .last()
            .is_some_and(|parameter| *parameter as usize >= parameters.len())
    {
        return Err(target);
    }
    let mut supplied = supplied.into_iter();
    let mut arguments = Vec::with_capacity(parameters.len());
    for (parameter, ty) in parameters.iter().copied().enumerate() {
        if omitted.contains(&(parameter as u32)) {
            arguments.push(
                ir.add_expr(IrExpr::Const(crate::ir::IrConst::zero_for_value_type(
                    ty.canonical_semantic(),
                ))),
            );
        } else {
            arguments.push(supplied.next().ok_or(target)?);
        }
    }
    if supplied.next().is_some() {
        return Err(target);
    }
    Ok(arguments)
}

pub(super) fn materialize_constructor_defaults(
    ir: &mut IrFile,
    parameters: &[crate::types::Ty],
    supplied: Vec<crate::ir::ExprId>,
    defaults: &[u32],
    prefix_count: u32,
    target: ExternalCallableId,
) -> Result<Vec<crate::ir::ExprId>, ExternalCallableId> {
    let omitted = defaults
        .iter()
        .map(|parameter| parameter.checked_add(prefix_count).ok_or(target))
        .collect::<Result<Vec<_>, _>>()?;
    materialize_omitted_arguments(ir, parameters, supplied, &omitted, target)
}
