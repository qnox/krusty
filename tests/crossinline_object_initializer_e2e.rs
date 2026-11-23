//! A `crossinline` lambda that a classpath inline function's anonymous object invokes while it is
//! constructed, from a property initializer or an `init` block, is inlined into the object's
//! constructor at the call. The original constructor reads the lambda from its parameter, as
//! kotlinc's does; the copy's constructor takes the lambda's captured values in its place and
//! holds the lambda's body where the invoke was.

use super::common;

const PROGRAM: &str = include_str!("box_regressions/multimodule/crossinlineObjectInitializer.kt");

/// The library and consumer sources of the shared portable program.
fn modules() -> (String, String) {
    let modules = krusty::conformance::split_modules(PROGRAM).expect("a two-module program");
    let source = |name: &str| {
        let module = modules
            .iter()
            .find(|module| module.name == name)
            .unwrap_or_else(|| panic!("module {name}"));
        assert_eq!(module.files.len(), 1);
        module.files[0].1.clone()
    };
    (source("lib"), source("main"))
}

#[test]
fn a_lambda_read_during_construction_runs_like_the_reference_compiler() {
    let (lib, main) = modules();
    let output = common::expect_box_run_against_ref("crossinline_object_initializer", &lib, &main)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}

/// The copies are kotlinc's, byte for byte: the constructor holds each lambda's body where the
/// original read the parameter, and the lambda's captured values are the copy's fields.
#[test]
fn the_object_copies_are_kotlincs() {
    let (lib, main) = modules();
    let classes = common::classes_against_kotlinc_lib("Initializing", &[("Lib.kt", &lib)], &main)
        .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        [
            "InitializingKt",
            "InitializingKt$counting$$inlined$counted$1",
            "InitializingKt$twice$$inlined$both$1",
        ]
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
