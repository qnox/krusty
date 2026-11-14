//! Escaping closure allocation and ordinary function-value invocation on both Wasm hosts.
//!
//! The source is the shared operand-order, local-return, and escaped-capture program.
//! Each harness process reproduces one release from `KotlinVersion::supported`.

use super::{focused_wasm_lane_available, WasmLane};
use crate::box_lane::{Lane, Outcome};
use krusty::kotlin_version::KotlinVersion;
use krusty_wasm::WasmTarget;

const SOURCE: &str = include_str!("box_regressions/immediateLambdaEscapingOperands.kt");
const STEM: &str = "immediateLambdaEscapingOperands";

#[test]
fn escaping_closures_allocate_and_invoke_on_both_wasm_hosts() {
    expect_function_program(SOURCE, STEM);
}

#[test]
fn ordinary_function_values_invoke_their_checked_signature_through_the_shared_prefix() {
    expect_function_program(
        include_str!("box_regressions/functionValueCalls.kt"),
        "functionValueCalls",
    );
}

fn expect_function_program(source: &str, stem: &str) {
    let releases = KotlinVersion::supported();
    let release = krusty::kotlin_version::target();
    assert!(
        releases.contains(&release),
        "Wasm closure execution reads a supported release, got {release} from {releases:?}"
    );
    crate::box_reference_classes::checked_box(source, stem).unwrap_or_else(|error| {
        panic!(
            "the JVM reference for Kotlin {release} must run the escaping closure program: {error}"
        )
    });
    for target in WasmTarget::ALL {
        let lane = WasmLane { target };
        assert!(
            focused_wasm_lane_available(&lane, stem),
            "{stem} on Kotlin {release} requires its {} lane",
            target.name()
        );
        assert_eq!(
            lane.compile_and_run(
                &std::env::temp_dir(),
                stem,
                &[(format!("{stem}.kt"), source.to_owned())],
                source,
            ),
            Outcome::Pass,
            "{stem} on Kotlin {release} / {}",
            target.name(),
        );
    }
}
