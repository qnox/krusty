//! A suspension point's erased `Object` result reaches its consumer unnarrowed: a `!!` checks it in
//! that slot, and only a consumer that needs a narrower type casts it.
// PLATFORM: complete JVM class inventories and raw artifact comparisons.

use super::common;

const NOT_NULL: &str = include_str!("box_regressions/suspendResultNotNullAssertion.kt");
const CONSUMERS: &str = include_str!("box_regressions/suspendResultErasedConsumers.kt");

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
fn the_complete_not_null_suspension_result_program_matches_kotlin() {
    assert_complete_module("suspendResultNotNullAssertion.kt", NOT_NULL);
}

#[test]
fn the_complete_erased_suspension_result_consumers_program_matches_kotlin() {
    assert_complete_module("suspendResultErasedConsumers.kt", CONSUMERS);
}
