//! The `@Metadata` kotlinc writes on a lambda's own class: the lambda's function (`<anonymous>`),
//! which `reflect()` and a lambda's `toString()` read when kotlin-reflect is present.

use super::*;
use crate::ir::{FunId, IrParameterRole, IrTypeParameter};

/// `d1` and `d2` for a lambda class of `function_type`, whose value parameters carry `names`: the
/// lambda's receiver, value parameters and result, without its context parameters.
pub(super) fn lambda_function_metadata(
    ir: &IrFile,
    function_type: Ty,
    names: &[String],
    type_parameters: &[IrTypeParameter],
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
    let (bytes, strings) = crate::metadata::lambda_function::build(
        &crate::metadata::lambda_function::LambdaFunction {
            receiver,
            parameters: &parameters,
            result: signature.ret,
            type_parameters,
            local_classifiers: &local_classifiers,
            enum_entry_bodies: &enum_entry_bodies,
            intersection_approximation: &approximate_intersection,
        },
    );
    (crate::metadata::encoding::bytes_to_strings(&bytes), strings)
}

/// The metadata of the class `-Xlambdas=class` makes of the lambda `node`, whose implementation
/// `function` takes `captures` captured values before the lambda's own parameters.
pub(super) fn class_mode_lambda_metadata(
    ir: &IrFile,
    node: ExprId,
    function: FunId,
    captures: usize,
    formatter: &JvmSignatureFormatter<'_>,
) -> Result<(Vec<String>, Vec<String>), String> {
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
            )
        })
        .map(|identity| {
            crate::jvm::parameter_names::metadata(identity)
                .map(str::to_owned)
                .ok_or_else(|| format!("lambda fid={function} has an unnamed value parameter"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(lambda_function_metadata(
        ir,
        function_type,
        &names,
        ir.lambda_type_parameters(function),
        formatter,
    ))
}
