//! The class `Signature` and `@Metadata` a `-Xlambdas=class` Kotlin function lambda carries.
//!
//! `Lambda.toString()` asks kotlin-reflect to read that record and render the function type.
//! Without it, reflection falls back to the raw `FunctionN` interface.

use crate::ir::type_reflection::LambdaClassProvenance;
use crate::ir::{IrFile, IrParameterRole};
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

/// The reflection record for a source lambda class. A synthesized adapter omits it. Any other
/// lambda, and a source lambda missing its function type or parameter identities, is an error:
/// those gaps are not proof that the class is an adapter.
pub(super) fn reflect(
    ir: &IrFile,
    expr: u32,
    impl_fn: u32,
    formatter: &JvmSignatureFormatter<'_>,
) -> Result<Option<ClassLambdaReflection>, String> {
    match ir.lambda_class_provenance(impl_fn) {
        Some(LambdaClassProvenance::SynthesizedAdapter) => return Ok(None),
        Some(LambdaClassProvenance::SourceFunction) => {}
        None => {
            return Err("a class-strategy lambda has no source-or-adapter provenance".to_string());
        }
    }
    let function_type = ir
        .logical_types
        .get(&expr)
        .copied()
        .ok_or("a source class lambda has no function type")?;
    let Ty::Fun(signature) = function_type.non_null() else {
        return Err("a source class lambda's type is not a function type".to_string());
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
    let names = value_parameter_names(ir, impl_fn, values.len())?
        .ok_or("a source class lambda has no parameter identities")?;
    let recorded = ir
        .recorded_lambda_type_parameters(impl_fn)
        .ok_or("a source class lambda does not publish its type parameters")?;
    let rendered = formatter
        .ty_at(&function_type.non_null(), Wildcards::Suppressed)
        .ok_or("a class lambda's function type has no JVM signature")?;
    let parameters = names
        .iter()
        .copied()
        .zip(values.iter().copied())
        .collect::<Vec<_>>();
    let origin = ir
        .lambda_origins
        .get(&impl_fn)
        .ok_or("a source class lambda has no source form")?;
    let local_classifiers = crate::jvm::local_classifiers::names(ir);
    let enum_entry_bodies = crate::jvm::local_classifiers::enum_entry_bodies(ir);
    let physical = super::super::metadata_method_signatures::lambda_invoke_descriptor(
        receiver,
        values,
        signature.ret,
    );
    let record_descriptor = super::super::metadata_method_signatures::requires_function_signature(
        receiver,
        values.iter().copied(),
        signature.ret,
        &physical,
        &local_classifiers,
    );
    let approximate_intersection = |ty| formatter.declaration_approximation(ty);
    let (bytes, strings) = crate::metadata::lambda_function::build(
        &crate::metadata::lambda_function::LambdaFunction {
            function_name: crate::metadata::lambda_function::function_name(origin.form),
            jvm_method: Some(crate::metadata::lambda_function::JvmMethod {
                name: "invoke",
                descriptor: record_descriptor.then_some(physical.as_str()),
            }),
            receiver,
            parameters: &parameters,
            result: signature.ret,
            type_parameters: recorded,
            local_classifiers: &local_classifiers,
            enum_entry_bodies: &enum_entry_bodies,
            intersection_approximation: &approximate_intersection,
        },
    )?;
    Ok(Some(ClassLambdaReflection {
        signature: format!("Lkotlin/jvm/internal/Lambda;{rendered}"),
        d1: crate::metadata::encoding::bytes_to_strings(&bytes),
        d2: strings,
    }))
}

/// Metadata names of the lambda's value parameters, in order. `None` when the implementation has
/// no parameter identities at all.
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
    matches!(
        role,
        IrParameterRole::Value | IrParameterRole::UnusedValue | IrParameterRole::DestructuredValue
    )
}
