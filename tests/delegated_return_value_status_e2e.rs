//! An `interface by` forwarder records the return-value status of the interface member it forwards.
//!
//! kotlinc's `@Metadata` for `class Name(...) : Comparable<String> by s` stores the stdlib
//! member's return-value status and `operator` modifier (function flags bits 8 and 16-17,
//! property flags bits 17-18) on the forwarder. krusty left them unset.

use super::common;

const SOURCE: &str = include_str!("box_regressions/delegatedBuiltinInterfaceMembers.kt");

#[test]
fn delegated_builtin_members_carry_the_status_of_the_members_they_forward() {
    common::assert_class_matches_kotlinc("DelegatedBuiltinInterfaceMembersStatus", SOURCE, "Name");
    common::expect_box_same_as_kotlinc(SOURCE, "DelegatedBuiltinInterfaceMembersStatusRun");
}

const MEMBER_EXTENSION_SOURCE: &str =
    include_str!("box_regressions/delegatedMemberExtensionProperty.kt");

#[test]
fn delegated_member_extension_property_records_its_forwarded_declaration_as_kotlinc_does() {
    common::assert_class_matches_kotlinc(
        "DelegatedMemberExtensionProperty",
        MEMBER_EXTENSION_SOURCE,
        "Delegating",
    );
    common::expect_box_same_as_kotlinc(
        MEMBER_EXTENSION_SOURCE,
        "DelegatedMemberExtensionPropertyRun",
    );
}

const MODULE_OVERRIDE_SOURCE: &str =
    include_str!("box_regressions/delegatedThroughModuleInterfaceOverride.kt");

#[test]
fn delegation_through_a_module_interface_that_overrides_a_stdlib_member_keeps_its_status() {
    common::assert_class_matches_kotlinc(
        "DelegatedThroughModuleInterfaceOverride",
        MODULE_OVERRIDE_SOURCE,
        "Delegating",
    );
    common::expect_box_same_as_kotlinc(
        MODULE_OVERRIDE_SOURCE,
        "DelegatedThroughModuleInterfaceOverrideRun",
    );
}

const INFIX_SOURCE: &str = include_str!("box_regressions/delegatedInfixThroughOverride.kt");

#[test]
fn delegated_infix_forwarder_keeps_the_modifier_an_override_inherits() {
    common::assert_class_matches_kotlinc(
        "DelegatedInfixThroughOverride",
        INFIX_SOURCE,
        "Delegating",
    );
    common::expect_box_same_as_kotlinc(INFIX_SOURCE, "DelegatedInfixThroughOverrideRun");
}

const OWN_MODIFIERS_SOURCE: &str = include_str!("box_regressions/delegatedOwnOperatorAndInfix.kt");

#[test]
fn delegated_forwarder_keeps_the_operator_and_infix_a_module_interface_member_declares() {
    common::assert_class_matches_kotlinc(
        "DelegatedOwnOperatorAndInfix",
        OWN_MODIFIERS_SOURCE,
        "Delegating",
    );
    common::expect_box_same_as_kotlinc(OWN_MODIFIERS_SOURCE, "DelegatedOwnOperatorAndInfixRun");
}
