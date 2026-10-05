//! The `@Metadata` kotlinc writes on a lambda's own class: the lambda's function (`<anonymous>`),
//! which `reflect()` and a lambda's `toString()` read when kotlin-reflect is present.

use super::*;
use crate::ir::{FunId, IrParameterRole, IrTypeParameter};

/// What a `-Xlambdas=class` lambda class records about the lambda's function.
#[derive(Clone, Debug)]
pub(super) struct LambdaClassMetadata {
    pub(super) d1: Vec<String>,
    pub(super) d2: Vec<String>,
    /// The descriptor of the class's typed `invoke`, when it declares one beside the erased
    /// `FunctionN.invoke`.
    pub(super) typed_invoke: Option<String>,
}

/// `d1` and `d2` for a lambda class of `function_type`, whose value parameters carry `names`: the
/// lambda's receiver, value parameters and result, without its context parameters. `typed_invoke`
/// is the descriptor of the class's typed `invoke`, which the metadata names.
pub(super) fn lambda_function_metadata(
    ir: &IrFile,
    lambda: FunId,
    function_type: Ty,
    names: &[String],
    type_parameters: &[IrTypeParameter],
    typed_invoke: Option<&str>,
    formatter: &JvmSignatureFormatter<'_>,
) -> (Vec<String>, Vec<String>) {
    let Ty::Fun(signature) = function_type.non_null() else {
        unreachable!("a lambda class has a function type")
    };
    let own = &signature.params[signature.context_count..];
    let (receiver, values) = match signature.has_receiver {
        true => (Some(own[0]), &own[1..]),
        false => (None, own),
    };
    assert_eq!(
        values.len(),
        names.len(),
        "a lambda names each value parameter of its function type"
    );
    let parameters: Vec<(&str, Ty)> = names
        .iter()
        .map(String::as_str)
        .zip(values.iter().copied())
        .collect();
    let local_classifiers = crate::jvm::local_classifiers::names(ir);
    let enum_entry_bodies = crate::jvm::local_classifiers::enum_entry_bodies(ir);
    let approximate_intersection = |ty| formatter.declaration_approximation(ty);
    // kotlinc's `requiresFunctionSignature`: the descriptor is recorded when a reader cannot
    // rebuild it from the declared types; the name always differs from `<anonymous>`.
    let jvm_signature = typed_invoke.map(|descriptor| {
        let recorded = crate::jvm::metadata_method_signatures::requires_function_signature(
            receiver,
            values.iter().copied(),
            signature.ret,
            descriptor,
            &local_classifiers,
        );
        ("invoke", recorded.then_some(descriptor))
    });
    let (bytes, strings) = crate::metadata::lambda_function::build(
        &crate::metadata::lambda_function::LambdaFunction {
            anonymous_function: ir
                .lambda_origins
                .get(&lambda)
                .is_some_and(|origin| origin.form == crate::ir::IrLambdaForm::AnonymousFunction),
            receiver,
            parameters: &parameters,
            result: signature.ret,
            type_parameters,
            local_classifiers: &local_classifiers,
            enum_entry_bodies: &enum_entry_bodies,
            intersection_approximation: &approximate_intersection,
            jvm_signature,
        },
    );
    (crate::metadata::encoding::bytes_to_strings(&bytes), strings)
}

/// The metadata of the class `-Xlambdas=class` makes of the lambda `node`, whose implementation
/// `function` takes `captures` captured values before the lambda's own parameters `own` and
/// returns `result` on the JVM. A lambda of a numbered function type gets kotlinc's typed `invoke`
/// over its own parameters, its scalar result boxed and a `Unit` result `void`; a big-arity one
/// only the packed bridge.
pub(super) fn class_mode_lambda_metadata(
    ir: &IrFile,
    node: ExprId,
    function: FunId,
    captures: usize,
    own: &[Ty],
    result: Ty,
    formatter: &JvmSignatureFormatter<'_>,
) -> Result<LambdaClassMetadata, String> {
    let function_type = ir
        .logical_types
        .get(&node)
        .copied()
        .ok_or_else(|| format!("lambda fid={function} has no recorded function type"))?;
    let identities = &ir
        .fn_params
        .get(&function)
        .ok_or_else(|| format!("lambda fid={function} has no parameter identities"))?
        .identities;
    let names = identities
        .get(captures..)
        .unwrap_or_default()
        .iter()
        .filter(|identity| {
            !matches!(
                identity.role,
                IrParameterRole::ExtensionReceiver
                    | IrParameterRole::ContextValue
                    | IrParameterRole::AnonymousContextParameter { .. }
                    | IrParameterRole::ContextReceiver { .. }
                    // A suspend lambda's continuation is not a parameter of its function type.
                    | IrParameterRole::Generated(_)
            )
        })
        .map(|identity| {
            crate::jvm::parameter_names::metadata(identity)
                .map(str::to_owned)
                .ok_or_else(|| format!("lambda fid={function} has an unnamed value parameter"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let type_parameters = ir
        .recorded_lambda_type_parameters(function)
        .ok_or_else(|| {
            format!(
                "lambda fid={function} names a type parameter declared outside this module, which \
             its class's metadata cannot describe yet"
            )
        })?;
    let numbered = !crate::jvm::names::uses_function_n(own.len());
    let typed_invoke = numbered.then(|| {
        let unit =
            matches!(function_type.non_null(), Ty::Fun(signature) if signature.ret == Ty::Unit);
        let result = match result {
            _ if unit => "V".to_string(),
            scalar if !descriptor_is_reference(&type_descriptor(scalar)) => {
                boxed_descriptor(scalar)
            }
            reference => type_descriptor(reference),
        };
        format!(
            "({}){result}",
            own.iter()
                .map(|ty| type_descriptor(*ty))
                .collect::<String>()
        )
    });
    let (d1, d2) = lambda_function_metadata(
        ir,
        function,
        function_type,
        &names,
        type_parameters,
        typed_invoke.as_deref(),
        formatter,
    );
    Ok(LambdaClassMetadata {
        d1,
        d2,
        typed_invoke,
    })
}
