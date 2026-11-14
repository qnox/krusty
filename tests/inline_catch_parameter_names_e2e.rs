//! Inlined catch parameters and locals keep their declared spelling plus one `$iv` per expansion;
//! an unused catch parameter is kotlinc's `<unused var>`.
// PLATFORM: complete JVM class inventories and raw artifact comparisons.

use super::common;

const UNUSED: &str = include_str!("box_regressions/unusedCatchParameterName.kt");
const INLINED: &str = include_str!("box_regressions/inlineCatchParameterNames.kt");

fn assert_complete_module(file: &str, source: &str) {
    let classes = common::classes_against_kotlinc_module(&[(file, source)]);
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
fn the_complete_unused_catch_program_matches_kotlin() {
    assert_complete_module("unusedCatchParameterName.kt", UNUSED);
}

#[test]
fn the_complete_inlined_catch_names_program_matches_kotlin() {
    assert_complete_module("inlineCatchParameterNames.kt", INLINED);
}

#[test]
fn the_inlined_catch_names_program_runs_like_kotlin() {
    common::expect_box_same_as_kotlinc(INLINED, "InlineCatchParameterNames");
}
