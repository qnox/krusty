//! Source-module singleton getter identity across complete source files.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty_wasm::WasmTarget;

// PLATFORM-SPECIFIC: execute both Wasm hosts; the entire Kotlin program and official JVM result
// also belong to the shared portable inventory, with the exact FILE stems retained.
#[test]
fn a_forward_source_singleton_has_one_instance_and_initializer() {
    let source = include_str!("box_regressions/sourceSingletonAcrossFiles.kt");
    let stem = "sourceSingletonAcrossFiles";
    crate::box_reference_classes::checked_box(source, stem)
        .expect("the complete source-singleton program returns OK on the official compiler");
    let modules = krusty::conformance::split_modules(source).expect("complete source topology");
    let units = krusty::conformance::module_units(&modules);
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(focused_wasm_lane_available(&lane, stem));
        assert_eq!(
            lane.compile_modules_and_run(&std::env::temp_dir(), stem, &units, source),
            Outcome::Pass,
            "{stem} on {}",
            target.name(),
        );
    }
}
