//! Diagnostic text and positions that differ between supported Kotlin reference versions.
//!
//! Every entry here is keyed by [`KotlinVersion`]: the reference compilers krusty reproduces do
//! not all spell a diagnostic the same way, and a user comparing krusty with their own kotlinc
//! must see that kotlinc's words. A call site asks this module rather than writing the text inline,
//! so supporting the next reference version means adding one row here, not hunting for strings.
//!
//! Each function names the kotlinc diagnostic it renders and the version that changed it: its text
//! (kotlinc's CLI rendering, the message template with its first letter lowered) or its [`Anchor`].

use crate::kotlin_version::{self, KotlinVersion};

fn since(version: KotlinVersion) -> bool {
    kotlin_version::at_least(version)
}

/// `NO_ACTUAL_FOR_EXPECT`, from IR actualization (`IrActualizationErrors`). 2.4.20 quotes the
/// declaration and the module and ends in a full stop.
pub fn no_actual_for_expect(name: &str, module_name: &str, target: &str) -> String {
    if since(KotlinVersion::V2_4_20) {
        format!(
            "the 'expect' declaration '{name}' has no 'actual' declaration in module \
             '<{module_name}> for {target}'."
        )
    } else {
        format!("expected {name} has no actual declaration in module <{module_name}> for {target}")
    }
}

/// `ACTUAL_WITHOUT_EXPECT`, with its platform-incompatibility suffix already rendered by the caller.
pub fn actual_without_expect(rendered: &str) -> String {
    if since(KotlinVersion::V2_4_20) {
        format!("'{rendered}' has no corresponding 'expect' declaration")
    } else {
        format!("'{rendered}' has no corresponding expected declaration")
    }
}

/// `NO_ACTUAL_CLASS_MEMBER_FOR_EXPECTED_CLASS`; `listing` starts with its own line break.
pub fn no_actual_class_members(rendered: &str, listing: &str) -> String {
    if since(KotlinVersion::V2_4_20) {
        format!("'{rendered}' has no corresponding members for 'expect' class members:{listing}")
    } else {
        format!("'{rendered}' has no corresponding members for expected class members:{listing}")
    }
}

/// `EXPECTED_DECLARATION_WITH_BODY`.
pub fn expected_declaration_with_body() -> &'static str {
    if since(KotlinVersion::V2_4_20) {
        "'expect' declaration cannot have a body."
    } else {
        "expected declaration cannot have a body."
    }
}

/// `EXPECTED_PROPERTY_INITIALIZER`.
pub fn expected_property_initializer() -> &'static str {
    if since(KotlinVersion::V2_4_20) {
        "'expect' property cannot have an initializer."
    } else {
        "expected property cannot have an initializer."
    }
}

/// `EXPECTED_DELEGATED_PROPERTY`.
pub fn expected_delegated_property() -> &'static str {
    if since(KotlinVersion::V2_4_20) {
        "'expect' property cannot be delegated."
    } else {
        "expected property cannot be delegated."
    }
}

/// `UNRESOLVED_REFERENCE` for a name looked up on an explicit receiver expression. From 2.4.20
/// kotlinc appends the receiver's type (`FOR_OPTIONAL_RECEIVER`) when it is a class-like type
/// without errors; `receiver` is that rendered type, or `None` when kotlinc would omit it.
pub fn unresolved_reference_on(name: &str, receiver: Option<&str>) -> String {
    match receiver {
        Some(receiver) if since(KotlinVersion::V2_4_20) => {
            format!("unresolved reference '{name}' on receiver of type '{receiver}'.")
        }
        _ => format!("unresolved reference '{name}'."),
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
pub fn no_value_for_parameter_anchor() -> Anchor {
    if since(KotlinVersion::V2_4_20) {
        Anchor::ReferencedNameByQualified
    } else {
        Anchor::ValueArguments
    }
}
