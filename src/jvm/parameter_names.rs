//! JVM projections of backend-neutral parameter identity and provenance.
//!
//! Kotlin metadata, local-variable tables, null assertions, and Java reflection do not share one
//! spelling contract. Keeping their projections separate prevents a physical JVM name from becoming
//! common-IR identity or leaking onto another class-file surface.

use crate::ir::{IrFile, IrGeneratedParameterRole, IrParameterIdentity, IrParameterRole};

/// Name of a JVM `LocalVariableTable` entry, or `None` for a genuinely unnamed parameter.
pub(super) fn local_variable(
    identity: &IrParameterIdentity,
    function_name: &str,
) -> Option<String> {
    if let Some(name) = &identity.source_name {
        return Some(name.clone());
    }
    match identity.role {
        IrParameterRole::Value | IrParameterRole::ContextValue => None,
        IrParameterRole::AnonymousContextParameter { .. } => None,
        IrParameterRole::ContextReceiver { ordinal } => {
            Some(format!("$context_receiver_{ordinal}"))
        }
        IrParameterRole::ExtensionReceiver => Some(format!("$this${function_name}")),
        IrParameterRole::CapturedValue { ordinal } => Some(format!("$capture{ordinal}")),
        IrParameterRole::CapturedReceiver { ordinal } => Some(format!("$this${ordinal}")),
        IrParameterRole::PropertySetterValue => Some("<set-?>".to_string()),
        IrParameterRole::Generated(role) => match role {
            IrGeneratedParameterRole::Positional { .. } => None,
            IrGeneratedParameterRole::Continuation => Some("$completion".to_string()),
            IrGeneratedParameterRole::HolderReceiver => Some("$this".to_string()),
            IrGeneratedParameterRole::ValueClassCarrier => Some("arg0".to_string()),
            IrGeneratedParameterRole::ValueClassEqualsOperand { ordinal } => {
                Some(value_class_equals_operand(ordinal).to_string())
            }
            IrGeneratedParameterRole::AccessorValue { ordinal } => Some(format!("value{ordinal}")),
            IrGeneratedParameterRole::ReferenceInvokeValue { ordinal } => {
                Some(format!("p{ordinal}"))
            }
        },
    }
}

/// A value parameter of the generic `invoke` bridge kotlinc writes for a callable-reference class.
/// It numbers them from one, unlike the specialized `invoke` it bridges to.
pub(super) fn reference_invoke_bridge_parameter(ordinal: u16) -> String {
    format!("p{}", ordinal + 1)
}

pub(super) fn value_class_equals_operand(ordinal: u8) -> &'static str {
    match ordinal {
        1 => "p1",
        2 => "p2",
        _ => panic!("a value-class equality operand is first or second"),
    }
}

/// Local-variable spellings for one complete physical parameter identity list. An entry may be
/// unnamed; that absence is preserved for class-file surfaces that support `name_index = 0` and
/// omitted LVT rows.
pub(super) fn function_locals(
    ir: &IrFile,
    function: u32,
    types: &[crate::types::Ty],
) -> Option<Vec<Option<String>>> {
    let identities = ir.function_parameter_identities(function)?;
    let anonymous = if anonymous_context_parameters_are_locals() {
        let semantic_types = function_semantic_parameter_types(ir, function, identities, types);
        disambiguated_anonymous_context_labels(identities, &semantic_types)
    } else {
        vec![None; identities.len()]
    };
    Some(
        identities
            .iter()
            .zip(anonymous)
            .map(|(identity, anonymous)| {
                anonymous.or_else(|| function_local_variable(ir, function, identity))
            })
            .collect(),
    )
}

/// Kotlin 2.4.20 names an anonymous context parameter by its generated label in the IR itself, so
/// the label is also its local-variable name; earlier releases left it unnamed there, publishing the
/// label only to reflection and null assertions.
fn anonymous_context_parameters_are_locals() -> bool {
    crate::kotlin_version::at_least(crate::kotlin_version::KotlinVersion::V2_4_20)
}

/// What separates a repeated anonymous context label from its ordinal: `$context-String$1` since
/// Kotlin 2.4.20, `$context-String#1` before it.
fn anonymous_context_ordinal_separator() -> char {
    if crate::kotlin_version::at_least(crate::kotlin_version::KotlinVersion::V2_4_20) {
        '$'
    } else {
        '#'
    }
}

/// JVM local-table spelling for a parameter of one exact common-IR function, apart from the
/// anonymous context labels [`function_locals`] adds for the whole parameter list.
///
/// A source extension declaration uses kotlinc's `$this$<function>` spelling. A receiver lambda's
/// static implementation instead uses `<this>`; the typed lambda edge selects that ABI surface,
/// without making common IR encode either JVM spelling.
fn function_local_variable(
    ir: &IrFile,
    function: u32,
    identity: &IrParameterIdentity,
) -> Option<String> {
    if matches!(identity.role, IrParameterRole::ExtensionReceiver) {
        if ir.lambda_own_params_from.contains_key(&function) {
            return Some("<this>".to_string());
        }
        let source_name = ir
            .fn_source_names
            .get(&function)
            .expect("an extension receiver retains its declaration source name");
        return Some(format!("$this${source_name}"));
    }
    local_variable(identity, "")
}

/// Kotlin metadata accepts only a declaration/producer-published semantic name.
pub(super) fn metadata(identity: &IrParameterIdentity) -> Option<&str> {
    match identity.role {
        IrParameterRole::AnonymousContextParameter { .. } => Some("<unused var>"),
        _ => identity.source_name.as_deref(),
    }
}

pub(super) fn metadata_context_kind(
    identity: &IrParameterIdentity,
) -> crate::types::ContextParameterKind {
    match identity.role {
        IrParameterRole::ContextValue => crate::types::ContextParameterKind::Named,
        IrParameterRole::AnonymousContextParameter { .. } => {
            crate::types::ContextParameterKind::Anonymous
        }
        IrParameterRole::ContextReceiver { .. } => {
            crate::types::ContextParameterKind::LegacyReceiver
        }
        _ => panic!("a metadata context prefix must retain its semantic role"),
    }
}

/// Name of one Java-reflection `MethodParameters` entry.
pub(super) fn method_parameter(
    identity: &IrParameterIdentity,
    function_name: &str,
) -> Option<String> {
    local_variable(identity, function_name)
}

fn anonymous_context_label(ty: crate::types::Ty) -> String {
    let stem = match ty.non_null() {
        crate::types::Ty::Obj(name, _) => name.nested_segment_ref(),
        crate::types::Ty::TyParam(name, _) => name,
        crate::types::Ty::Unit => "Unit",
        crate::types::Ty::Fun(_) => "Function",
        crate::types::Ty::Nothing => "Nothing",
        unexpected => panic!("anonymous context parameter has no JVM type label: {unexpected:?}"),
    };
    format!("$context-{stem}")
}

fn disambiguated_anonymous_context_labels(
    identities: &[IrParameterIdentity],
    types: &[crate::types::Ty],
) -> Vec<Option<String>> {
    assert_eq!(identities.len(), types.len());
    let bases = identities
        .iter()
        .zip(types)
        .map(|(identity, ty)| {
            matches!(
                identity.role,
                IrParameterRole::AnonymousContextParameter { .. }
            )
            .then(|| anonymous_context_label(*ty))
        })
        .collect::<Vec<_>>();
    let mut totals = std::collections::HashMap::<&str, usize>::new();
    for base in bases.iter().flatten() {
        *totals.entry(base).or_default() += 1;
    }
    let mut seen = std::collections::HashMap::<&str, usize>::new();
    bases
        .iter()
        .map(|base| {
            let base = base.as_deref()?;
            if totals[base] == 1 {
                Some(base.to_owned())
            } else {
                let ordinal = seen.entry(base).or_default();
                *ordinal += 1;
                Some(format!(
                    "{base}{}{ordinal}",
                    anonymous_context_ordinal_separator()
                ))
            }
        })
        .collect()
}

fn function_semantic_parameter_types(
    ir: &IrFile,
    function: u32,
    identities: &[IrParameterIdentity],
    physical_types: &[crate::types::Ty],
) -> Vec<crate::types::Ty> {
    let declared = ir
        .vc_declared_sigs
        .get(&function)
        .map(|(_, params, _)| params.as_slice())
        .or_else(|| {
            ir.suspend_declared_sigs
                .get(&function)
                .map(|(params, _)| params.as_slice())
        })
        .or_else(|| {
            ir.signatures
                .get(&function)
                .map(|signature| signature.params.as_slice())
        })
        .or_else(|| {
            ir.member_semantic_sigs
                .get(&function)
                .map(|(params, _)| params.as_slice())
        });
    identities
        .iter()
        .enumerate()
        .map(|(index, identity)| match identity.role {
            IrParameterRole::AnonymousContextParameter { ordinal } => match declared {
                Some(params) => *params
                    .get(ordinal as usize)
                    .expect("an anonymous context parameter retains its declared semantic type"),
                // These maps are installed only when a representation pass changes a declaration
                // parameter. Without one, the function's physical list is still the checked common-
                // IR semantic list; there is no second spelling or descriptor path to consult.
                None => physical_types[index],
            },
            _ => physical_types[index],
        })
        .collect()
}

fn constructor_identities(arguments: &[crate::ir::IrCtorArg]) -> Vec<IrParameterIdentity> {
    let mut context_ordinal = 0u32;
    arguments
        .iter()
        .enumerate()
        .map(|(physical_ordinal, argument)| {
            let identity = match argument.context_kind {
                crate::types::ContextParameterKind::Named => IrParameterIdentity::context_value(
                    argument
                        .name
                        .as_deref()
                        .expect("a named classifier context parameter retains its source name"),
                ),
                crate::types::ContextParameterKind::Anonymous => {
                    IrParameterIdentity::anonymous_context_parameter(context_ordinal)
                }
                crate::types::ContextParameterKind::LegacyReceiver => {
                    IrParameterIdentity::context_receiver(context_ordinal)
                }
                crate::types::ContextParameterKind::None => match argument.name.as_deref() {
                    Some(name) => IrParameterIdentity::source(name),
                    None => IrParameterIdentity::generated(
                        IrGeneratedParameterRole::Positional {
                            ordinal: physical_ordinal as u32,
                        },
                        None,
                    ),
                },
            };
            if argument.context_kind != crate::types::ContextParameterKind::None {
                context_ordinal += 1;
            }
            identity
        })
        .collect()
}

fn constructor_anonymous_labels(
    arguments: &[crate::ir::IrCtorArg],
    identities: &[IrParameterIdentity],
) -> Vec<Option<String>> {
    let semantic_types = arguments
        .iter()
        .map(|argument| argument.declared_ty.unwrap_or(argument.ty))
        .collect::<Vec<_>>();
    disambiguated_anonymous_context_labels(identities, &semantic_types)
}

pub(super) fn constructor_method_parameters(
    arguments: &[crate::ir::IrCtorArg],
) -> Vec<Option<String>> {
    let identities = constructor_identities(arguments);
    let anonymous = constructor_anonymous_labels(arguments, &identities);
    identities
        .iter()
        .enumerate()
        .map(|(index, identity)| {
            anonymous[index]
                .clone()
                .or_else(|| method_parameter(identity, "<init>"))
        })
        .collect()
}

pub(super) fn constructor_local_variables(
    arguments: &[crate::ir::IrCtorArg],
) -> Vec<Option<String>> {
    let identities = constructor_identities(arguments);
    let anonymous = if anonymous_context_parameters_are_locals() {
        constructor_anonymous_labels(arguments, &identities)
    } else {
        vec![None; identities.len()]
    };
    identities
        .iter()
        .zip(anonymous)
        .map(|(identity, anonymous)| anonymous.or_else(|| local_variable(identity, "<init>")))
        .collect()
}

pub(super) fn constructor_assertions(arguments: &[crate::ir::IrCtorArg]) -> Vec<Option<String>> {
    let identities = constructor_identities(arguments);
    let anonymous = constructor_anonymous_labels(arguments, &identities);
    identities
        .iter()
        .enumerate()
        .map(|(index, identity)| anonymous[index].clone().or_else(|| assertion(identity)))
        .collect()
}

pub(super) fn function_method_parameters(
    ir: &IrFile,
    function: u32,
    types: &[crate::types::Ty],
) -> Option<Vec<Option<String>>> {
    let identities = ir.function_parameter_identities(function)?;
    let semantic_types = function_semantic_parameter_types(ir, function, identities, types);
    let anonymous = disambiguated_anonymous_context_labels(identities, &semantic_types);
    Some(
        identities
            .iter()
            .enumerate()
            .map(|(index, identity)| {
                anonymous[index]
                    .clone()
                    .or_else(|| function_local_variable(ir, function, identity))
            })
            .collect(),
    )
}

pub(super) fn function_assertions(
    ir: &IrFile,
    function: u32,
    types: &[crate::types::Ty],
) -> Option<Vec<Option<String>>> {
    let identities = ir.function_parameter_identities(function)?;
    let semantic_types = function_semantic_parameter_types(ir, function, identities, types);
    let anonymous = disambiguated_anonymous_context_labels(identities, &semantic_types);
    // kotlinc replaces a function whose signature the value-class ABI mangles (or moves to a static
    // `-impl`) with a copy whose extension receiver is an ordinary parameter, named as the
    // receiver's local variable. Its guard then quotes that name rather than `<this>`.
    let replaced = ir
        .vc_declared_sigs
        .get(&function)
        .zip(ir.functions.get(function as usize))
        .is_some_and(|((declared, ..), physical)| *declared != physical.name);
    Some(
        identities
            .iter()
            .enumerate()
            .map(|(index, identity)| {
                anonymous[index].clone().or_else(|| match identity.role {
                    IrParameterRole::ExtensionReceiver if replaced => {
                        function_local_variable(ir, function, identity)
                    }
                    _ => assertion(identity),
                })
            })
            .collect(),
    )
}

/// Text passed to `Intrinsics.checkNotNullParameter` for one checked parameter.
pub(super) fn assertion(identity: &IrParameterIdentity) -> Option<String> {
    match identity.role {
        IrParameterRole::ExtensionReceiver => Some("<this>".to_string()),
        _ => local_variable(identity, ""),
    }
}

/// Parameter identity written to coroutine `@DebugMetadata`.
pub(super) fn debug_metadata(
    identity: &IrParameterIdentity,
    function_name: &str,
) -> Option<String> {
    local_variable(identity, function_name)
}

/// Debug name for an override/bridge parameter published before common IR is built.
pub(super) fn resolved_local_variable(
    identity: &crate::fir::ResolvedParameterIdentity,
    function_name: &str,
) -> Option<String> {
    match identity {
        crate::fir::ResolvedParameterIdentity::Source(name) => Some(name.to_string()),
        crate::fir::ResolvedParameterIdentity::Unnamed { .. } => None,
        crate::fir::ResolvedParameterIdentity::ContextValue { source_name, .. } => {
            Some(source_name.to_string())
        }
        crate::fir::ResolvedParameterIdentity::AnonymousContextParameter { .. } => None,
        crate::fir::ResolvedParameterIdentity::LegacyContextReceiver { ordinal } => {
            Some(format!("$context_receiver_{ordinal}"))
        }
        crate::fir::ResolvedParameterIdentity::ExtensionReceiver => {
            Some(format!("$this${function_name}"))
        }
        crate::fir::ResolvedParameterIdentity::PropertySetterValue => Some("<set-?>".to_string()),
        crate::fir::ResolvedParameterIdentity::SuspendCompletion => Some("$completion".to_string()),
    }
}

/// Local-variable spellings for a provider-published parameter list. Anonymous context labels are
/// derived from the declaration's semantic types, never from erased bridge descriptors.
pub(super) fn resolved_local_variables(
    identities: &[crate::fir::ResolvedParameterIdentity],
    semantic_types: &[crate::types::Ty],
    function_name: &str,
) -> Vec<Option<String>> {
    let anonymous = if anonymous_context_parameters_are_locals() {
        resolved_anonymous_context_labels(identities, semantic_types)
    } else {
        vec![None; identities.len()]
    };
    identities
        .iter()
        .zip(anonymous)
        .map(|(identity, anonymous)| {
            anonymous.or_else(|| resolved_local_variable(identity, function_name))
        })
        .collect()
}

fn resolved_anonymous_context_labels(
    identities: &[crate::fir::ResolvedParameterIdentity],
    semantic_types: &[crate::types::Ty],
) -> Vec<Option<String>> {
    assert_eq!(identities.len(), semantic_types.len());
    let projected = identities
        .iter()
        .map(|identity| match identity {
            crate::fir::ResolvedParameterIdentity::AnonymousContextParameter { ordinal } => {
                IrParameterIdentity::anonymous_context_parameter(*ordinal)
            }
            _ => IrParameterIdentity::generated(
                IrGeneratedParameterRole::Positional { ordinal: 0 },
                None,
            ),
        })
        .collect::<Vec<_>>();
    disambiguated_anonymous_context_labels(&projected, semantic_types)
}

pub(super) fn resolved_method_parameters(
    identities: &[crate::fir::ResolvedParameterIdentity],
    semantic_types: &[crate::types::Ty],
    function_name: &str,
) -> Vec<Option<String>> {
    let anonymous = resolved_anonymous_context_labels(identities, semantic_types);
    identities
        .iter()
        .enumerate()
        .map(|(index, identity)| {
            anonymous[index]
                .clone()
                .or_else(|| resolved_local_variable(identity, function_name))
        })
        .collect()
}

pub(super) fn resolved_assertions(
    identities: &[crate::fir::ResolvedParameterIdentity],
    semantic_types: &[crate::types::Ty],
    function_name: &str,
) -> Vec<Option<String>> {
    let anonymous = resolved_anonymous_context_labels(identities, semantic_types);
    identities
        .iter()
        .enumerate()
        .map(|(index, identity)| {
            anonymous[index].clone().or_else(|| match identity {
                crate::fir::ResolvedParameterIdentity::ExtensionReceiver => {
                    Some("<this>".to_string())
                }
                _ => resolved_local_variable(identity, function_name),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        FnParamInfo, IrFunction, IrGeneratedParameterRole, IrParameterCheck, IrParameterIdentity,
    };

    #[test]
    fn each_jvm_surface_projects_the_receiver_for_its_own_contract() {
        let receiver = IrParameterIdentity::extension_receiver();
        assert_eq!(
            local_variable(&receiver, "inspect"),
            Some("$this$inspect".to_string())
        );
        assert_eq!(assertion(&receiver), Some("<this>".to_string()));
        assert_eq!(metadata(&receiver), None);
    }

    #[test]
    fn an_unnamed_generated_parameter_never_acquires_a_positional_name() {
        let generated = IrParameterIdentity::generated(
            IrGeneratedParameterRole::Positional { ordinal: 3 },
            None,
        );
        assert_eq!(local_variable(&generated, "inspect"), None);
        assert_eq!(method_parameter(&generated, "inspect"), None);
        assert_eq!(metadata(&generated), None);
    }

    #[test]
    fn receiver_lambda_uses_its_backend_debug_surface() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "transform$lambda$0".to_string(),
            params: vec![crate::types::Ty::String],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        ir.fn_params.insert(
            function,
            FnParamInfo::identities(vec![IrParameterIdentity::extension_receiver()]),
        );
        ir.lambda_own_params_from.insert(function, 0);

        assert_eq!(
            function_locals(&ir, function, &[crate::types::Ty::String]),
            Some(vec![Some("<this>".to_string())])
        );
    }

    #[test]
    fn declared_extension_receiver_uses_only_its_published_source_name() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "renamed-physical".to_string(),
            params: vec![crate::types::Ty::String],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        ir.fn_params.insert(
            function,
            FnParamInfo::identities(vec![IrParameterIdentity::extension_receiver()]),
        );
        ir.fn_source_names.insert(function, "transform".to_string());

        assert_eq!(
            function_locals(&ir, function, &[crate::types::Ty::String]),
            Some(vec![Some("$this$transform".to_string())])
        );
    }

    #[test]
    #[should_panic(expected = "an extension receiver retains its declaration source name")]
    fn declared_extension_receiver_never_uses_a_physical_function_name() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "renamed-physical".to_string(),
            params: vec![crate::types::Ty::String],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None],
        });
        ir.fn_params.insert(
            function,
            FnParamInfo::identities(vec![IrParameterIdentity::extension_receiver()]),
        );

        let _ = function_locals(&ir, function, &[crate::types::Ty::String]);
    }

    #[test]
    fn anonymous_context_label_uses_the_declared_value_class_not_its_carrier() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "inspect-erased".to_string(),
            params: vec![crate::types::Ty::String, crate::types::Ty::String],
            ret: crate::types::Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None::<IrParameterCheck>; 2],
        });
        ir.fn_params.insert(
            function,
            FnParamInfo::identities(vec![
                IrParameterIdentity::anonymous_context_parameter(0),
                IrParameterIdentity::source("value"),
            ]),
        );
        ir.vc_declared_sigs.insert(
            function,
            (
                "inspect".to_string(),
                vec![
                    crate::types::Ty::obj("demo/Wrapped"),
                    crate::types::Ty::String,
                ],
                crate::types::Ty::Unit,
            ),
        );

        assert_eq!(
            function_method_parameters(
                &ir,
                function,
                &[crate::types::Ty::String, crate::types::Ty::String],
            ),
            Some(vec![
                Some("$context-Wrapped".to_string()),
                Some("value".to_string()),
            ])
        );
        assert_eq!(
            function_locals(
                &ir,
                function,
                &[crate::types::Ty::String, crate::types::Ty::String],
            ),
            Some(vec![
                anonymous_context_parameters_are_locals()
                    .then(|| Some("$context-Wrapped".to_string()))
                    .flatten(),
                Some("value".to_string()),
            ])
        );
    }
}
