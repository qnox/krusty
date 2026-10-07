//! The class `Signature` and `@Metadata` a `-Xlambdas=class` Kotlin function lambda carries.
//!
//! `Lambda.toString()` asks kotlin-reflect to read that record and render the function type.
//! Without it, reflection falls back to the raw `FunctionN` interface. The record names the
//! class's typed `invoke`, which the class declares beside the erased `FunctionN.invoke` bridge.

use crate::ir::type_reflection::type_parameters_named_by;
use crate::ir::type_reflection::LambdaClassProvenance;
use crate::ir::{IrFile, IrParameterRole, IrTypeParameter};
use crate::types::Ty;

use super::signature_formatter::{JvmSignatureFormatter, Wildcards};
use super::{boxed_descriptor, descriptor_is_reference, type_descriptor};

/// What kotlin-reflect reads off the lambda class.
#[derive(Clone, Debug)]
pub(super) struct ClassLambdaReflection {
    /// `Lkotlin/jvm/internal/Lambda;Lkotlin/jvm/functions/FunctionN<…>;`
    pub(super) signature: String,
    pub(super) d1: Vec<String>,
    pub(super) d2: Vec<String>,
    /// The descriptor of the typed `invoke` the class declares beside the erased
    /// `FunctionN.invoke`. Absent for a big-arity lambda, whose class implements only the packed
    /// `invoke(Object[])`.
    pub(super) typed_invoke: Option<TypedInvoke>,
}

/// The typed `invoke` a lambda class declares.
#[derive(Clone, Debug)]
pub(super) struct TypedInvoke {
    pub(super) descriptor: String,
    /// Its generic `Signature`, when a parameter or the result names a type parameter or a
    /// parameterized type.
    pub(super) signature: Option<String>,
}

/// The lambda's own parameters and result as its implementation method takes and returns them.
pub(super) struct ClassLambdaInvoke<'a> {
    pub(super) own: &'a [Ty],
    pub(super) result: Ty,
}

/// The reflection record for a source lambda class. A synthesized adapter omits it. Any other
/// lambda, and a source lambda missing its function type or parameter identities, is an error:
/// those gaps are not proof that the class is an adapter.
pub(super) fn reflect(
    ir: &IrFile,
    expr: u32,
    impl_fn: u32,
    invoke: ClassLambdaInvoke<'_>,
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
    let recorded = ir
        .recorded_lambda_type_parameters(impl_fn)
        .ok_or("a source class lambda does not publish its type parameters")?;
    let function_type = approximate_unpublished_type_parameters(function_type, recorded);
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
            explicit_suspend: origin.explicit_suspend,
            receiver,
            parameters: &parameters,
            result: signature.ret,
            type_parameters: recorded,
            local_classifiers: &local_classifiers,
            enum_entry_bodies: &enum_entry_bodies,
            intersection_approximation: &approximate_intersection,
        },
    )?;
    let arity = usize::from(receiver.is_some()) + values.len();
    let typed_invoke = (!crate::jvm::names::uses_function_n(arity)
        && implementation_invoke_descriptor(&invoke, signature.ret) == physical)
        .then(|| typed_invoke(formatter, receiver, values, signature.ret, physical))
        .transpose()?;
    Ok(Some(ClassLambdaReflection {
        signature: format!("Lkotlin/jvm/internal/Lambda;{rendered}"),
        d1: crate::metadata::encoding::bytes_to_strings(&bytes),
        d2: strings,
        typed_invoke,
    }))
}

/// The typed `invoke` with descriptor `physical`, and its generic `Signature` when it differs from
/// the descriptor. A boxed primitive or `void` result keeps its descriptor spelling.
fn typed_invoke(
    formatter: &JvmSignatureFormatter<'_>,
    receiver: Option<Ty>,
    values: &[Ty],
    result: Ty,
    physical: String,
) -> Result<TypedInvoke, String> {
    let mut signature = String::from("(");
    for parameter in receiver.iter().chain(values) {
        signature.push_str(
            &formatter
                .method_ty(parameter, Wildcards::Declared)
                .ok_or("a class lambda's parameter has no JVM signature")?,
        );
    }
    signature.push(')');
    let descriptor_result = physical
        .rsplit_once(')')
        .map(|(_, result)| result)
        .ok_or("a lambda invoke descriptor has a result")?;
    let generic_result = formatter
        .method_ty(&result, Wildcards::Suppressed)
        .ok_or("a class lambda's result has no JVM signature")?;
    if generic_result == type_descriptor(result) {
        signature.push_str(descriptor_result);
    } else {
        signature.push_str(&generic_result);
    }
    Ok(TypedInvoke {
        signature: (signature != physical).then_some(signature),
        descriptor: physical,
    })
}

/// The typed `invoke` the class can declare over its implementation method: the implementation's
/// own parameters, its scalar result boxed and a `Unit` result `void`. A lambda whose
/// implementation is represented differently from its function type (a value class parameter)
/// keeps only the erased bridge.
fn implementation_invoke_descriptor(invoke: &ClassLambdaInvoke<'_>, result: Ty) -> String {
    let parameters = invoke
        .own
        .iter()
        .map(|ty| type_descriptor(*ty))
        .collect::<String>();
    let result = match invoke.result {
        _ if result == Ty::Unit => "V".to_string(),
        scalar if !descriptor_is_reference(&type_descriptor(scalar)) => boxed_descriptor(scalar),
        reference => type_descriptor(reference),
    };
    format!("({parameters}){result}")
}

/// `function_type` with each type parameter the checker did not publish for the lambda replaced
/// by its bound. A builder-inferred lambda can name a type parameter of the library function that
/// inferred it, an inference variable outside the lambda's scope; kotlinc's declaration
/// approximation replaces it the same way.
pub(super) fn approximate_unpublished_type_parameters(
    function_type: Ty,
    published: &[IrTypeParameter],
) -> Ty {
    let mut named = Vec::new();
    type_parameters_named_by(function_type, &mut named);
    let mut unpublished = Vec::new();
    let mut index = 0;
    while let Some(&name) = named.get(index) {
        index += 1;
        match published
            .iter()
            .find(|parameter| parameter.semantic_name == name)
        {
            Some(parameter) => {
                for &(bound, _) in &parameter.bounds {
                    type_parameters_named_by(bound, &mut named);
                }
            }
            None => unpublished.push(name),
        }
    }
    approximate_type_parameters(function_type, &unpublished)
}

/// `ty` with each type parameter in `names` replaced by its bound, itself approximated.
fn approximate_type_parameters(ty: Ty, names: &[&str]) -> Ty {
    if names.is_empty() {
        return ty;
    }
    let approximate = |ty: Ty| approximate_type_parameters(ty, names);
    match ty {
        Ty::TyParam(name, bound) if names.contains(&name) => approximate(*bound),
        Ty::Fun(signature) => Ty::fun_with_shape(
            signature
                .params
                .iter()
                .map(|&parameter| approximate(parameter))
                .collect(),
            approximate(signature.ret),
            signature.context_count,
            signature.has_receiver,
            signature.suspend,
        ),
        Ty::Obj(name, arguments) if !arguments.is_empty() => Ty::obj_args_name(
            name,
            &arguments
                .iter()
                .map(|&argument| approximate(argument))
                .collect::<Vec<_>>(),
        ),
        Ty::Nullable(inner) => Ty::nullable(approximate(*inner)),
        Ty::DefinitelyNotNull(inner) => approximate(*inner).non_null(),
        Ty::PlatformNullable(inner) => Ty::platform_nullable(approximate(*inner)),
        Ty::InProjection(inner) => Ty::in_projection(approximate(*inner)),
        Ty::OutProjection(inner) => Ty::out_projection(approximate(*inner)),
        Ty::Intersection(parts) => Ty::intersection(
            &parts
                .iter()
                .map(|&part| approximate(part))
                .collect::<Vec<_>>(),
        ),
        _ => ty,
    }
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
