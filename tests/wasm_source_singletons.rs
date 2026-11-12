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
    let (files, java) = krusty::conformance::split_files(source);
    assert!(
        java.is_empty(),
        "the source-singleton program has no Java files"
    );
    let files = files
        .into_iter()
        .map(|(file_stem, text)| (format!("{file_stem}.kt"), text))
        .collect::<Vec<_>>();
    assert_eq!(
        files
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["SourceSingletonUse.kt", "SourceSingletonDeclarations.kt"],
        "the using file stays first",
    );
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(focused_wasm_lane_available(&lane, stem));
        assert_eq!(
            lane.compile_and_run(&std::env::temp_dir(), stem, &files, source),
            Outcome::Pass,
            "{stem} on {}",
            target.name(),
        );
    }
}
