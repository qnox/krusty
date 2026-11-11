//! Complete ordinary and generic inner-instance edges use the same source on both Wasm targets.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty_wasm::WasmTarget;

#[test]
// PLATFORM-SPECIFIC: this driver provisions and executes both Wasm hosts. The complete Kotlin
// program remains shared, and its official JVM runtime oracle uses the identical source.
fn inner_instance_edges_match_the_official_runtime_with_receiver_effects_once() {
    let source = include_str!("box_regressions/InnerEnclosingInstanceEdges.kt");
    let stem = "InnerEnclosingInstanceEdges";
    crate::box_reference_classes::checked_box(source, stem).expect(
        "the exact Kotlin compiler must accept and run the complete inner-instance program",
    );
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(focused_wasm_lane_available(&lane, stem));
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
