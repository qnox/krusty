//! A context parameter whose type is an inner class. Resolving `Outer.Inner` (or an unqualified
//! `Inner` inside a generic outer) consults the declaration's implicit receivers for the outer's
//! type arguments; the declaration's own context parameters are not in scope for their own types,
//! so that lookup must not ask for them again. Both programs are also box regressions, so every
//! backend lane runs them.

use super::common;

#[test]
fn a_context_parameter_of_an_inner_type_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/contextParameterInnerType.kt"),
        "ContextInnerType",
    );
}

#[test]
fn a_context_parameter_of_an_unqualified_inner_type_in_a_generic_outer_matches_kotlinc() {
    common::expect_box_same_as_kotlinc(
        include_str!("box_regressions/contextParameterGenericInnerType.kt"),
        "ContextGenericInnerType",
    );
}
