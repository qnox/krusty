//! Transitional JVM rendering of typed common-IR parameter identities.
//!
//! The common contract carries no JVM spelling. This module keeps the existing consumers building
//! while each JVM surface is migrated to its own exact projection.

use crate::ir::{IrFile, IrGeneratedParameterRole, IrParameterIdentity, IrParameterRole};

pub(super) fn legacy(identity: &IrParameterIdentity, function_name: &str) -> String {
    if let Some(name) = &identity.source_name {
        return name.clone();
    }
    match identity.role {
        IrParameterRole::Value | IrParameterRole::ContextValue => String::new(),
        IrParameterRole::ContextReceiver { ordinal } => format!("$context_receiver_{ordinal}"),
        IrParameterRole::ExtensionReceiver => format!("$this${function_name}"),
        IrParameterRole::CapturedValue { ordinal } => format!("$capture{ordinal}"),
        IrParameterRole::CapturedReceiver { ordinal } => format!("$this${ordinal}"),
        IrParameterRole::PropertySetterValue => "<set-?>".to_string(),
        IrParameterRole::Generated(role) => match role {
            IrGeneratedParameterRole::Positional { ordinal } => format!("p{ordinal}"),
            IrGeneratedParameterRole::Continuation => "$completion".to_string(),
            IrGeneratedParameterRole::HolderReceiver => "$this".to_string(),
            IrGeneratedParameterRole::ValueClassCarrier => "arg0".to_string(),
            IrGeneratedParameterRole::AccessorValue { ordinal } => format!("value{ordinal}"),
        },
    }
}

pub(super) fn function(ir: &IrFile, function: u32) -> Option<Vec<String>> {
    let name = &ir.functions.get(function as usize)?.name;
    Some(
        ir.function_parameter_identities(function)?
            .iter()
            .map(|identity| legacy(identity, name))
            .collect(),
    )
}

pub(super) fn assertion(identity: &IrParameterIdentity, function_name: &str) -> Option<String> {
    if matches!(identity.role, IrParameterRole::ExtensionReceiver) {
        Some("<this>".to_string())
    } else {
        identity
            .source_name
            .clone()
            .or_else(|| Some(legacy(identity, function_name)))
            .filter(|name| !name.is_empty())
    }
}
