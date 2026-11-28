//! A program's own console output on each Wasm target is the target stdlib's, as kotlinc-wasm
//! 2.4.21 runs it: `wasm-js` prints through the stdlib's `js("console.log(…)")` functions, so
//! every `print` is a line of its own, and `wasm-wasi` writes through `fd_write`, where a `null`
//! message writes nothing.

use super::*;

#[test]
fn console_output_on_each_wasm_target_is_the_target_stdlib_s() {
    let source = include_str!("box_regressions/consolePrinting.kt");
    let stem = "consolePrinting";
    super::super::box_reference_classes::checked_box(source, stem)
        .expect("the exact Kotlin compiler must run the complete console fixture");
    let large = "x".repeat(200_000);
    for (target, printed) in [
        (
            WasmTarget::Js,
            format!("text\n\nno newline\n42\n[1, 2]\nc\nnull\n{large}\n"),
        ),
        (
            WasmTarget::Wasi,
            format!("text\n\nno newline42\n[1, 2]\nc\n{large}\n"),
        ),
    ] {
        let lane = WasmLane { target };
        if !focused_wasm_lane_available(&lane, stem) {
            return;
        }
        let backend = lane
            .backend(source)
            .expect("the fixture's compiler options");
        let program = box_lane::compile(
            &[(format!("{stem}.kt"), source.to_string())],
            source,
            lane.test_target(),
            &backend,
            "box",
            DECLINE_PREFIX,
        )
        .unwrap_or_else(|outcome| panic!("{stem} on {}: {outcome:?}", target.name()));
        let directory = std::env::temp_dir().join(format!(
            "{stem}-console-{}-{}",
            target.name(),
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("create the case directory");
        for (name, bytes) in &program.artifacts {
            std::fs::write(directory.join(name), bytes).expect("write an artifact");
        }
        let output = Command::new("node")
            .arg("--no-warnings")
            .arg(directory.join("box.mjs"))
            .output()
            .expect("run the module");
        let _ = std::fs::remove_dir_all(&directory);
        assert_eq!(
            (
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ),
            (
                true,
                format!("{printed}{}OK", krusty::backend::BOX_RESULT_FRAME),
                String::new(),
            ),
            "{stem} on {}",
            target.name()
        );
    }
}
