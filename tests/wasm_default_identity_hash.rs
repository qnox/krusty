//! Default identity hash storage across source files and runtime-owned Throwable layouts.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty_wasm::WasmTarget;

#[test]
fn ordinary_and_runtime_object_hashes_are_stable_on_both_wasm_hosts() {
    expect_box(
        include_str!("box_regressions/defaultIdentityHashThrowable.kt"),
        "defaultIdentityHashThrowable",
    );
}

// PLATFORM-SPECIFIC: provision and execute both Wasm hosts; the complete Kotlin inputs and
// official JVM result remain shared with the canonical portable inventory.
pub(super) fn expect_box(source: &str, stem: &str) {
    crate::box_reference_classes::checked_box(source, stem)
        .expect("the complete identity hash program must return OK on the official compiler");
    let (units, _) = krusty::conformance::split_files(source)
        .expect("the complete identity hash fixture must have valid file directives");
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(
            focused_wasm_lane_available(&lane, stem),
            "{stem} requires its {} lane",
            target.name()
        );
        assert_eq!(
            lane.compile_and_run(&std::env::temp_dir(), stem, &units, source),
            Outcome::Pass,
            "{stem} on {}",
            target.name()
        );
    }
}
