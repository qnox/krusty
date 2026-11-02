//! An inlined extension lambda's receiver reuses the local that already holds the inlined callee's
//! receiver instead of storing a second copy of it.
// PLATFORM: complete JVM class inventory and raw artifact comparison.

use super::common;

const PROGRAM: &str =
    include_str!("fixtures/apply_receiver_single_store/applyReceiverSingleStore.kt");

#[test]
fn the_complete_apply_receiver_program_matches_kotlin() {
    let classes =
        common::classes_against_kotlinc_module(&[("applyReceiverSingleStore.kt", PROGRAM)]);
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
fn the_apply_receiver_program_runs_like_kotlin() {
    common::expect_box_same_as_kotlinc(PROGRAM, "ApplyReceiverSingleStore");
}
