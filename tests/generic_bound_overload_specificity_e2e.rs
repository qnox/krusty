//! A generic overload whose type parameter is bounded by a classifier is more specific than one
//! whose type parameter is free.

use super::common;

#[test]
fn a_bounded_generic_overload_beats_a_free_generic_overload() {
    const SOURCE: &str = include_str!("box_regressions/genericBoundNarrowsOverloadSpecificity.kt");
    common::expect_box_same_as_kotlinc(SOURCE, "generic-bound-overload-specificity");
}

/// A bound that narrows one declaration does not make it more specific when the other parameter
/// is an unrelated fixed classifier: both overloads accept the call, neither accepts the other's
/// parameters, so kotlinc reports the ambiguity.
#[test]
fn an_unrelated_fixed_parameter_keeps_a_bounded_overload_ambiguous() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "Main.kt",
            "interface Source<out T>\n\
             interface Left\n\
             interface Right\n\
             class Both : Left, Right\n\
             fun <T : Any> choose(source: Source<T>, side: Left): String = \"left\"\n\
             fun <A : Number> choose(source: Source<A>, side: Right): String = \"right\"\n\
             fun test(source: Source<Int>, both: Both) = choose(source, both)\n",
        )],
        &[],
    );
}

/// An enclosing classifier's parameter is rigid while two declarations are compared: `X : Right`
/// is not `Right`, so the bounded overload does not dominate the one taking `X`, and an argument
/// of exactly `X` does not make the first overload better: kotlinc reports the ambiguity.
#[test]
fn an_enclosing_type_parameter_stays_rigid_in_the_comparison() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "Main.kt",
            "interface Right\n\
             class Source<out T>(val value: T)\n\
             class Holder<X : Right> {\n\
                 fun <T : Any> choose(source: Source<T>, side: X): CharSequence = \"general\"\n\
                 fun <A : Number> choose(source: Source<A>, side: Right): String = \"bounded\"\n\
                 fun test(source: Source<Int>, side: X): Any = choose(source, side)\n\
             }\n",
        )],
        &[],
    );
}
