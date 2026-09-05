//! Target-neutral semantic validation of declaration headers.

use crate::ast::{FunDecl, PropDecl};
use crate::diag::Span;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DeclarationDiagnostic {
    pub span: Span,
    pub message: &'static str,
}

pub(crate) fn infix_declaration_diagnostics(
    function: &FunDecl,
    member: bool,
) -> Vec<DeclarationDiagnostic> {
    if !function.is_infix() {
        return Vec::new();
    }

    let mut diagnostics = Vec::new();
    if !member && function.receiver.is_none() {
        diagnostics.push(DeclarationDiagnostic {
            span: function.span,
            message: "'infix' modifier is inapplicable on this function: must be a member or an extension function",
        });
    }
    let value_parameters = &function.params[function.context_count..];
    if value_parameters.len() != 1 {
        diagnostics.push(DeclarationDiagnostic {
            span: function.span,
            message: "'infix' modifier is inapplicable on this function: must have a single value parameter",
        });
        return diagnostics;
    }
    let parameter = &value_parameters[0];
    if parameter.default.is_some() {
        diagnostics.push(DeclarationDiagnostic {
            span: parameter.ty.span,
            message: "'infix' modifier is inapplicable on this function: parameter must have no default value",
        });
    }
    if parameter.is_vararg {
        diagnostics.push(DeclarationDiagnostic {
            span: parameter.ty.span,
            message:
                "'infix' modifier is inapplicable on this function: parameter must not be vararg",
        });
    }
    diagnostics
}

pub(crate) fn interface_property_diagnostics(property: &PropDecl) -> Vec<DeclarationDiagnostic> {
    if property.init.is_none()
        && property.delegate.is_none()
        && property.explicit_backing_field.is_none()
        && !property.getter_reads_field
    {
        return Vec::new();
    }
    vec![DeclarationDiagnostic {
        span: property.span,
        message: "property in interface cannot have a backing field.",
    }]
}
