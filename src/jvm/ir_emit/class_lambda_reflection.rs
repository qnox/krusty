//! The class `Signature` and `@Metadata` a `-Xlambdas=class` Kotlin function lambda carries.
//!
//! `Lambda.toString()` asks kotlin-reflect to read that record and render the function type.
//! Without it, reflection falls back to the raw `FunctionN` interface.

use crate::ir::type_reflection::type_parameters_named_by;
use crate::ir::{IrFile, IrParameterRole, IrTypeParameter};
use crate::types::Ty;

use super::signature_formatter::{JvmSignatureFormatter, Wildcards};

/// What kotlin-reflect reads off the lambda class.
#[derive(Clone, Debug)]
pub(super) struct ClassLambdaReflection {
    /// `Lkotlin/jvm/internal/Lambda;Lkotlin/jvm/functions/FunctionN<…>;`
    pub(super) signature: String,
    pub(super) d1: Vec<String>,
    pub(super) d2: Vec<String>,
}

/// The reflection record for the lambda expression `expr`, when its implementation is a source
/// function whose parameters and type parameters are recorded. A synthesized adapter that has
/// neither returns `Ok(None)` and keeps the class it already had.
pub(super) fn reflect(
    ir: &IrFile,
    expr: u32,
    impl_fn: u32,
    formatter: &JvmSignatureFormatter<'_>,
) -> Result<Option<ClassLambdaReflection>, String> {
    let Some(function_type) = ir.logical_types.get(&expr).copied() else {
        return Ok(None);
    };
    let Ty::Fun(signature) = function_type.non_null() else {
        return Ok(None);
    };
    let own = signature
        .params
        .get(signature.context_count..)
        .ok_or("a class lambda's context parameters run past its function type")?;
    let (receiver, values) = if signature.has_receiver {
        let (receiver, values) = own
            .split_first()
            .ok_or("a class lambda's receiver is missing from its function type")?;
        (Some(*receiver), values)
    } else {
        (None, own)
    };
    let Some(names) = value_parameter_names(ir, impl_fn, values.len())? else {
        return Ok(None);
    };
    let recorded = ir.recorded_lambda_type_parameters(impl_fn).unwrap_or(&[]);
    if !type_parameters_available(signature.ret, receiver, values, recorded) {
        return Ok(None);
    }
    let rendered = formatter
        .ty_at(&function_type.non_null(), Wildcards::Suppressed)
        .ok_or("a class lambda's function type has no JVM signature")?;
    let parameters = names
        .iter()
        .copied()
        .zip(values.iter().copied())
        .collect::<Vec<_>>();
    let local_classifiers = crate::jvm::local_classifiers::names(ir);
    let enum_entry_bodies = crate::jvm::local_classifiers::enum_entry_bodies(ir);
    let approximate_intersection = |ty| formatter.declaration_approximation(ty);
    let (bytes, strings) = crate::metadata::lambda_function::build(
        &crate::metadata::lambda_function::LambdaFunction {
            receiver,
            parameters: &parameters,
            result: signature.ret,
            type_parameters: recorded,
            local_classifiers: &local_classifiers,
            enum_entry_bodies: &enum_entry_bodies,
            intersection_approximation: &approximate_intersection,
        },
    );
    Ok(Some(ClassLambdaReflection {
        signature: format!("Lkotlin/jvm/internal/Lambda;{rendered}"),
        d1: crate::metadata::encoding::bytes_to_strings(&bytes),
        d2: strings,
    }))
}

/// Metadata names of the lambda's value parameters, in order. `None` when the implementation has
/// no parameter identities at all (a synthesized adapter, not a source lambda).
fn value_parameter_names<'a>(
    ir: &'a IrFile,
    impl_fn: u32,
    value_count: usize,
) -> Result<Option<Vec<&'a str>>, String> {
    let Some(identities) = ir.function_parameter_identities(impl_fn) else {
        return Ok(None);
    };
    let own_from = ir
        .lambda_own_params_from
        .get(&impl_fn)
        .copied()
        .unwrap_or(0) as usize;
    let own = identities
        .get(own_from..)
        .ok_or("a class lambda's capture prefix is longer than its parameter identities")?;
    let mut names = Vec::new();
    for identity in own {
        if !is_value_parameter(identity.role) {
            continue;
        }
        let Some(name) = crate::jvm::parameter_names::metadata(identity) else {
            return Err("a class lambda value parameter has no metadata name".to_string());
        };
        names.push(name);
    }
    if names.len() != value_count {
        return Err(format!(
            "a class lambda records {value_count} value parameters and {} metadata names",
            names.len()
        ));
    }
    Ok(Some(names))
}

fn is_value_parameter(role: IrParameterRole) -> bool {
    !matches!(
        role,
        IrParameterRole::ExtensionReceiver
            | IrParameterRole::ContextValue
            | IrParameterRole::AnonymousContextParameter { .. }
            | IrParameterRole::ContextReceiver { .. }
    )
}

/// Whether every type parameter the function type names, directly or through a bound, was
/// recorded for this lambda. A synthesized function that names an enclosing parameter without
/// that record is not a source lambda's metadata.
fn type_parameters_available(
    result: Ty,
    receiver: Option<Ty>,
    values: &[Ty],
    recorded: &[IrTypeParameter],
) -> bool {
    let mut names = Vec::new();
    type_parameters_named_by(result, &mut names);
    if let Some(receiver) = receiver {
        type_parameters_named_by(receiver, &mut names);
    }
    for &value in values {
        type_parameters_named_by(value, &mut names);
    }
    let mut index = 0;
    while index < names.len() {
        let name = names[index];
        let Some(parameter) = recorded
            .iter()
            .find(|parameter| parameter.semantic_name == name)
        else {
            return false;
        };
        for &(bound, _) in &parameter.bounds {
            type_parameters_named_by(bound, &mut names);
        }
        index += 1;
    }
    true
}
