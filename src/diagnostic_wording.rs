//! Diagnostic text that Kotlin reference releases have spelled differently.
//!
//! A user comparing krusty with their own kotlinc must see that kotlinc's words. A call site asks
//! this module rather than writing the text inline, so a reference release that rewords one of
//! these diagnostics changes one function here, not strings across the resolver.
//!
//! Each function names the kotlinc diagnostic it renders: kotlinc's CLI rendering, the message
//! template with its first letter lowered.

use crate::diag::{names, NamedMessage};

/// `NO_ACTUAL_FOR_EXPECT`, from IR actualization (`IrActualizationErrors`): it quotes the
/// declaration and the module and ends in a full stop.
pub fn no_actual_for_expect(name: &str, module_name: &str, target: &str) -> String {
    format!(
        "the 'expect' declaration '{name}' has no 'actual' declaration in module \
         '<{module_name}> for {target}'."
    )
}

/// `ACTUAL_WITHOUT_EXPECT`, with its platform-incompatibility suffix already rendered by the caller.
pub fn actual_without_expect(rendered: &str) -> String {
    format!("'{rendered}' has no corresponding 'expect' declaration")
}

/// `NO_ACTUAL_CLASS_MEMBER_FOR_EXPECTED_CLASS`; `listing` starts with its own line break.
pub fn no_actual_class_members(rendered: &str, listing: &str) -> String {
    format!("'{rendered}' has no corresponding members for 'expect' class members:{listing}")
}

/// `EXPECTED_DECLARATION_WITH_BODY`.
pub fn expected_declaration_with_body() -> &'static str {
    "'expect' declaration cannot have a body."
}

/// `EXPECTED_PROPERTY_INITIALIZER`.
pub fn expected_property_initializer() -> &'static str {
    "'expect' property cannot have an initializer."
}

/// `EXPECTED_DELEGATED_PROPERTY`.
pub fn expected_delegated_property() -> &'static str {
    "'expect' property cannot be delegated."
}

/// A selected callable reference rejected by its receiver constraint: the diagnostic names that
/// candidate.
pub fn callable_reference_receiver_mismatch(candidate: &str) -> String {
    format!("candidate '{candidate}' is inapplicable because of a receiver type mismatch.")
}

/// `UNRESOLVED_REFERENCE` for a name kotlinc resolves without a receiver type to report.
pub fn unresolved_reference(name: &str) -> NamedMessage {
    NamedMessage {
        name: names::UNRESOLVED_REFERENCE,
        message: format!("Unresolved reference '{name}'."),
    }
}

/// `ARGUMENT_TYPE_MISMATCH`: an argument of type `actual` passed where `expected` is declared.
pub fn argument_type_mismatch(actual: &str, expected: &str) -> NamedMessage {
    NamedMessage {
        name: names::ARGUMENT_TYPE_MISMATCH,
        message: format!(
            "Argument type mismatch: actual type is '{actual}', but '{expected}' was expected."
        ),
    }
}

/// `UNSAFE_CALL`: a member that exists only on the non-null type, called on a nullable receiver.
pub fn unsafe_call(receiver: &str) -> NamedMessage {
    NamedMessage {
        name: names::UNSAFE_CALL,
        message: format!(
            "Only safe (?.) or non-null asserted (!!.) calls are allowed on a nullable receiver \
             of type '{receiver}'."
        ),
    }
}

/// `UNRESOLVED_IMPORT`: an import path segment that names nothing. kotlinc renders it as it renders
/// `UNRESOLVED_REFERENCE`.
pub fn unresolved_import(segment: &str) -> NamedMessage {
    NamedMessage {
        name: names::UNRESOLVED_IMPORT,
        ..unresolved_reference(segment)
    }
}

/// `UNRESOLVED_REFERENCE` for a name looked up on an explicit receiver expression. kotlinc appends
/// the receiver's type (`FOR_OPTIONAL_RECEIVER`) when it is a class-like type without errors;
/// `receiver` is that rendered type, or `None` when kotlinc would omit it.
pub fn unresolved_reference_on(name: &str, receiver: Option<&str>) -> NamedMessage {
    let message = match receiver {
        Some(receiver) => {
            format!("Unresolved reference '{name}' on receiver of type '{receiver}'.")
        }
        _ => return unresolved_reference(name),
    };
    NamedMessage {
        name: names::UNRESOLVED_REFERENCE,
        message,
    }
}

/// `UNRESOLVED_REFERENCE` for an operator convention looked up on a receiver. kotlinc appends the receiver's type just as it does for an ordinary explicit-receiver lookup.
pub fn unresolved_operator_reference_on(
    name: &str,
    operator: &str,
    receiver: Option<&str>,
) -> NamedMessage {
    let message = match receiver {
        Some(receiver) => format!(
            "Unresolved reference '{name}' for operator '{operator}' on receiver of type \
             '{receiver}'."
        ),
        _ => format!("Unresolved reference '{name}' for operator '{operator}'."),
    };
    NamedMessage {
        name: names::UNRESOLVED_REFERENCE,
        message,
    }
}
