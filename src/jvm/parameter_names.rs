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
        },
    }
}

pub(super) fn value_class_equals_operand(ordinal: u8) -> &'static str {
    match ordinal {
        1 => "p1",
        2 => "p2",
        _ => panic!("a value-class equality operand is first or second"),
    }
}

/// Complete local-variable names for a function surface that requires every physical parameter to
/// be debug-visible. `None` distinguishes an absent/unnameable contract from an empty parameter list.
pub(super) fn required_function_locals(ir: &IrFile, function: u32) -> Option<Vec<String>> {
    let function_shape = ir.functions.get(function as usize)?;
    ir.function_parameter_identities(function)?
        .iter()
        .map(|identity| local_variable(identity, &function_shape.name))
        .collect()
}

/// Kotlin metadata accepts only a declaration/producer-published semantic name.
pub(super) fn metadata(identity: &IrParameterIdentity) -> Option<&str> {
    identity.source_name.as_deref()
}

/// Name of one Java-reflection `MethodParameters` entry.
pub(super) fn method_parameter(
    identity: &IrParameterIdentity,
    function_name: &str,
) -> Option<String> {
    local_variable(identity, function_name)
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
) -> Option<String> {
    match identity {
        crate::fir::ResolvedParameterIdentity::Source(name) => Some(name.to_string()),
        crate::fir::ResolvedParameterIdentity::CompilerGenerated(_) => None,
        crate::fir::ResolvedParameterIdentity::PropertySetterValue => Some("<set-?>".to_string()),
        crate::fir::ResolvedParameterIdentity::SuspendCompletion => Some("$completion".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IrGeneratedParameterRole, IrParameterIdentity};

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
}
