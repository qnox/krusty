//! Whole builtin-value hash contracts at both Wasm representation boundaries.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty::wasm::WasmTarget;

#[test]
fn builtin_value_hashes_match_kotlin_including_utf16_and_ieee_bits() {
    let source = include_str!("box_regressions/wasmValueHash.kt");
    let stem = "wasmValueHash";
    crate::box_reference_classes::checked_box(source, stem)
        .expect("the exact Kotlin compiler must accept and run the complete value hash program");
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(
            focused_wasm_lane_available(&lane, stem),
            "the owned value hash program requires its Wasm lane"
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
