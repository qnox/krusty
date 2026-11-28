//! Diagnostic text and positions that differ between supported Kotlin reference versions.
//!
//! Every entry here is keyed by [`KotlinVersion`]: the reference compilers krusty reproduces do
//! not all spell a diagnostic the same way, and a user comparing krusty with their own kotlinc
//! must see that kotlinc's words. A call site asks this module rather than writing the text inline,
//! so supporting the next reference version means adding one row here, not hunting for strings.
//!
//! Each function names the kotlinc diagnostic it renders and the version that changed it: its text
//! (kotlinc's CLI rendering, the message template with its first letter lowered) or its [`Anchor`].

use crate::diag::{names, NamedMessage};
use crate::kotlin_version::{KotlinVersion, ReferenceProfile};

/// `NO_ACTUAL_FOR_EXPECT`, from IR actualization (`IrActualizationErrors`). 2.4.20 quotes the
/// declaration and the module and ends in a full stop.
pub fn no_actual_for_expect(
    reference: ReferenceProfile,
    name: &str,
    module_name: &str,
    target: &str,
) -> String {
    if reference.at_least(KotlinVersion::V2_4_20) {
        format!(
            "the 'expect' declaration '{name}' has no 'actual' declaration in module \
             '<{module_name}> for {target}'."
        )
    } else {
        format!("expected {name} has no actual declaration in module <{module_name}> for {target}")
    }
}

/// `ACTUAL_WITHOUT_EXPECT`, with its platform-incompatibility suffix already rendered by the caller.
pub fn actual_without_expect(reference: ReferenceProfile, rendered: &str) -> String {
    if reference.at_least(KotlinVersion::V2_4_20) {
        format!("'{rendered}' has no corresponding 'expect' declaration")
    } else {
        format!("'{rendered}' has no corresponding expected declaration")
    }
}

/// `NO_ACTUAL_CLASS_MEMBER_FOR_EXPECTED_CLASS`; `listing` starts with its own line break.
pub fn no_actual_class_members(
    reference: ReferenceProfile,
    rendered: &str,
    listing: &str,
) -> String {
    if reference.at_least(KotlinVersion::V2_4_20) {
        format!("'{rendered}' has no corresponding members for 'expect' class members:{listing}")
    } else {
        format!("'{rendered}' has no corresponding members for expected class members:{listing}")
    }
}

/// `EXPECTED_DECLARATION_WITH_BODY`.
pub fn expected_declaration_with_body(reference: ReferenceProfile) -> &'static str {
    if reference.at_least(KotlinVersion::V2_4_20) {
        "'expect' declaration cannot have a body."
    } else {
        "expected declaration cannot have a body."
    }
}

/// `EXPECTED_PROPERTY_INITIALIZER`.
pub fn expected_property_initializer(reference: ReferenceProfile) -> &'static str {
    if reference.at_least(KotlinVersion::V2_4_20) {
        "'expect' property cannot have an initializer."
    } else {
        "expected property cannot have an initializer."
    }
}

/// `EXPECTED_DELEGATED_PROPERTY`.
pub fn expected_delegated_property(reference: ReferenceProfile) -> &'static str {
    if reference.at_least(KotlinVersion::V2_4_20) {
        "'expect' property cannot be delegated."
    } else {
        "expected property cannot be delegated."
    }
}

/// A selected callable reference rejected by its receiver constraint. From 2.4.20 the diagnostic
/// names that candidate; older compilers use the unresolved-reference wrong-receiver heading.
pub fn callable_reference_receiver_mismatch(
    reference: ReferenceProfile,
    candidate: &str,
) -> String {
    if reference.at_least(KotlinVersion::V2_4_20) {
        format!("candidate '{candidate}' is inapplicable because of a receiver type mismatch.")
    } else {
        "unresolved reference. None of the following candidates is applicable because of a receiver type mismatch:".to_owned()
    }
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

/// `UNINITIALIZED_VARIABLE`: a read of a local that no path has definitely assigned yet.
pub fn uninitialized_variable(name: &str) -> NamedMessage {
    NamedMessage {
        name: names::UNINITIALIZED_VARIABLE,
        message: format!("Variable '{name}' must be initialized."),
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

/// `UNRESOLVED_REFERENCE` for a name looked up on an explicit receiver expression. From 2.4.20
/// kotlinc appends the receiver's type (`FOR_OPTIONAL_RECEIVER`) when it is a class-like type
/// without errors; `receiver` is that rendered type, or `None` when kotlinc would omit it.
pub fn unresolved_reference_on(
    reference: ReferenceProfile,
    name: &str,
    receiver: Option<&str>,
) -> NamedMessage {
    let message = match receiver {
        Some(receiver) if reference.at_least(KotlinVersion::V2_4_20) => {
            format!("Unresolved reference '{name}' on receiver of type '{receiver}'.")
        }
        _ => return unresolved_reference(name),
    };
    NamedMessage {
        name: names::UNRESOLVED_REFERENCE,
        message,
    }
}

/// `UNRESOLVED_REFERENCE` for an operator convention looked up on a receiver. From 2.4.20
/// kotlinc appends the receiver's type just as it does for an ordinary explicit-receiver lookup.
pub fn unresolved_operator_reference_on(
    reference: ReferenceProfile,
    name: &str,
    operator: &str,
    receiver: Option<&str>,
) -> NamedMessage {
    let message = match receiver {
        Some(receiver) if reference.at_least(KotlinVersion::V2_4_20) => format!(
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

/// Where kotlinc anchors a diagnostic in the source, named after the positioning strategy it uses
/// (`SourceElementPositioningStrategies`). A table row that moves a diagnostic between releases
/// returns one of these, and the resolver computes the span from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// `VALUE_ARGUMENTS`: the call's argument list, at the argument the failure recovers to.
    ValueArguments,
    /// `REFERENCED_NAME_BY_QUALIFIED`: the callee's name, after any qualifier.
    ReferencedNameByQualified,
}

/// `NO_VALUE_FOR_PARAMETER`: 2.4.20 moved it from the argument list to the callee's name.
pub fn no_value_for_parameter_anchor(reference: ReferenceProfile) -> Anchor {
    if reference.at_least(KotlinVersion::V2_4_20) {
        Anchor::ReferencedNameByQualified
    } else {
        Anchor::ValueArguments
    }
}

/// An inapplicable member call on a receiver that also has same-name extensions: 2.4.20 keeps
/// climbing the tower past the member, so when the extensions are inapplicable too it reports
/// NONE_APPLICABLE for the member and the extensions together, at the callee's name. Earlier
/// releases stop at the member and report its own error.
pub fn inapplicable_member_joins_extensions(reference: ReferenceProfile) -> bool {
    reference.at_least(KotlinVersion::V2_4_20)
}
