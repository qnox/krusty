//! krusty's Kotlin/Wasm programs reproduce kotlinc-wasm's defects by default.
//!
//! PLATFORM-SPECIFIC: `ArrayListAddAllGrowsBeforeReadingItsArgument` is a defect of the Kotlin/Native
//! and Kotlin/Wasm standard library, whose `ArrayList.addAll` grows the list before it reads the
//! argument. `tests/kotlinc_defects_e2e.rs` pins kotlinc's answer on every platform and krusty's on
//! the JVM and Native, through krusty's CLI. krusty compiles Kotlin/Wasm in process only, so this
//! owning test runs the same program under each target's exact kotlinc-wasm and requires the
//! product program to observe what it observes.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty::compilation_target::CompilationTarget;
use krusty_wasm::WasmTarget;

const LIST_ADD_ALL_OF_ITS_RANGE: &str = include_str!("kotlinc_defects/listAddAllOfItsRange.kt");

#[test]
fn a_list_appending_a_range_of_itself_raises_as_kotlinc_wasm_does() {
    let stem = "wasmListAddAllOfItsRange";
    for (target, compilation_target) in [
        (WasmTarget::Js, CompilationTarget::WasmJs),
        (WasmTarget::Wasi, CompilationTarget::WasmWasi),
    ] {
        let lane = WasmLane { target };
        assert!(
            focused_wasm_lane_available(&lane, stem),
            "{stem} requires its {} lane",
            target.name()
        );
        let work = std::env::temp_dir().join(format!("{stem}-{}", target.name()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();
        let reference_source = work.join(format!("{stem}.kt"));
        std::fs::write(
            &reference_source,
            format!("{LIST_ADD_ALL_OF_ITS_RANGE}\nfun main() {{\n    println(box())\n}}\n"),
        )
        .unwrap();
        let reference = crate::reference_wasm_program::run_wasm_reference_program(
            compilation_target,
            &work,
            &[reference_source],
        )
        .expect("the exact Kotlin/Wasm reference compiler is provisioned")
        .unwrap_or_else(|error| panic!("{} reference: {error}", target.name()));
        let expected = reference.trim_end();
        assert_eq!(
            expected,
            "raised [1, 2, 3, null, null]",
            "kotlinc-wasm {} no longer has the defect",
            target.name()
        );
        let program = LIST_ADD_ALL_OF_ITS_RANGE.replacen("fun box()", "fun observed()", 1);
        let source = format!(
            "// FILE: {stem}.kt\n{program}\nfun box(): String {{\n    val seen = observed()\n    \
             return if (seen == \"{expected}\") \"OK\" else \"FAIL \" + seen\n}}\n"
        );
        let (units, _) = krusty::conformance::split_files(&source)
            .expect("the defect program names its one file");
        assert_eq!(
            lane.compile_and_run(&work, stem, &units, &source),
            Outcome::Pass,
            "{stem} on {} against the reference's `{expected}`",
            target.name()
        );
        std::fs::remove_dir_all(work).unwrap();
    }
}
