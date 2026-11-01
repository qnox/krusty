//! A suspend lambda class spells its `FunctionN` supertype's arguments in generic-argument mode:
//! every declaration-site variance below them is written (`Continuation<-List<+Box>>`), while
//! `invoke` keeps the ordinary parameter rules.
// PLATFORM: complete JVM class inventories and raw artifact comparisons.

use super::common;

const PROGRAM: &str =
    include_str!("fixtures/suspend_lambda_supertype_wildcards/SuspendLambdaSupertypeWildcards.kt");

#[test]
fn the_complete_suspend_lambda_supertype_program_matches_kotlin() {
    let classes =
        common::classes_against_kotlinc_module(&[("SuspendLambdaSupertypeWildcards.kt", PROGRAM)]);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>(),
        "complete class inventory"
    );
    let differences = classes
        .reference
        .iter()
        .filter_map(|(name, reference)| {
            let ours = &classes.krusty[name];
            (reference != ours).then(|| common::exact_class_difference(name, reference, ours))
        })
        .collect::<Vec<_>>();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

#[test]
fn the_suspend_lambda_supertype_program_runs_like_kotlin() {
    common::expect_box_same_as_kotlinc(PROGRAM, "SuspendLambdaSupertypeWildcards");
}
