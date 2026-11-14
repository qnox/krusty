//! Erased `ArrayList` equality, hash, rendering, and element reads and writes.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty_wasm::WasmTarget;

fn expect_array_list(source: &str, stem: &str) {
    crate::box_reference_classes::checked_box(source, stem)
        .expect("the exact Kotlin compiler must accept and run the list program");
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(
            focused_wasm_lane_available(&lane, stem),
            "the owned list program requires its Wasm lane"
        );
        assert_eq!(
            lane.compile_and_run(
                &std::env::temp_dir(),
                stem,
                &[(format!("{stem}.kt"), source.to_owned())],
                source,
            ),
            Outcome::Pass,
            "{stem} on {}",
            target.name(),
        );
    }
}

#[test]
fn array_list_reads_and_writes_round_trip_elements() {
    expect_array_list(
        include_str!("box_regressions/arrayListReadWrite.kt"),
        "arrayListReadWrite",
    );
}

#[test]
fn erased_array_lists_compare_hash_and_render_structurally() {
    expect_array_list(
        include_str!("box_regressions/arrayListErasedAnyMembers.kt"),
        "arrayListErasedAnyMembers",
    );
}
