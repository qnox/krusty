//! A same-module inline function with a `noinline` suspend function-typed parameter is inlined from
//! its compiled body, as kotlinc inlines it: the parameter's argument is a value the body only
//! passes on, so the call's suspend lambda is the call site's own object and nothing in the callee
//! suspends inside the caller.

use super::common;

/// The two-file program, shared with every box lane.
const PROGRAM: &str = include_str!("box_regressions/inlineSameModuleNoinlineSuspendValues.kt");

/// `Declaration.kt` and `Caller.kt`, split from [`PROGRAM`] as the box gate splits it.
fn sources() -> Vec<(String, String)> {
    let (kotlin, java) = krusty::conformance::split_files(PROGRAM);
    assert!(java.is_empty(), "the program has no Java files");
    kotlin
        .into_iter()
        .map(|(stem, text)| (format!("{stem}.kt"), text))
        .collect()
}

fn files(sources: &[(String, String)]) -> Vec<(&str, &str)> {
    sources
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect()
}

#[test]
fn a_noinline_suspend_parameters_callee_is_inlined_from_the_compiled_body_like_kotlinc() {
    let sources = sources();
    let files = files(&sources);
    assert_eq!(
        files.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        ["Declaration.kt", "Caller.kt"]
    );
    let classes = common::classes_against_kotlinc_module(&files);
    assert_eq!(
        classes.krusty.keys().collect::<Vec<_>>(),
        classes.reference.keys().collect::<Vec<_>>()
    );
    for name in classes.reference.keys() {
        assert!(
            classes.krusty.get(name) == classes.reference.get(name),
            "{name} differs from kotlinc's bytes"
        );
    }
}

#[test]
fn a_noinline_suspend_parameters_callee_inlined_from_the_compiled_body_runs() {
    let sources = sources();
    common::expect_box_ok_files_with_stdlib(&files(&sources), "CallerKt");
}
