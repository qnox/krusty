//! Receiver reads retain their checked binding stability through safe-cast lowering.
// PLATFORM: complete JVM class inventories, local storage and raw artifact comparisons.

use super::common;

const BARE_CAST: &str = include_str!("box_regressions/bareCastTargetArguments.kt");
const RECEIVER_READS: &str = include_str!("fixtures/implicit_receiver_stability/ReceiverReads.kt");

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
fn the_complete_bare_cast_module_matches_kotlin() {
    assert_complete_module("bareCastTargetArguments.kt", BARE_CAST);
}

#[test]
fn the_complete_bare_cast_program_runs_like_kotlin() {
    common::expect_box_same_as_kotlinc(BARE_CAST, "BareReceiverCast");
}

#[test]
fn own_captured_and_inline_receiver_artifacts_match_kotlin() {
    assert_complete_module("ReceiverReads.kt", RECEIVER_READS);
}

#[test]
fn receiver_snapshots_and_shared_cells_run_like_kotlin() {
    common::expect_box_same_as_kotlinc(RECEIVER_READS, "ReceiverReads");
}
