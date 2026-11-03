//! An `internal` member of another module is invisible at the call site, so an applicable
//! extension takes the operator call, the indexed read and the call, as for a private member.
//!
//! The program is the shared box regression
//! `tests/box_regressions/multimodule/internalOperatorMemberYieldsToExtension.kt`, which constructs
//! the dependency class in the main module. This guard runs it on the JVM: kotlinc compiles and
//! runs it first, and krusty's `box()` result must equal kotlinc's. The factory regression
//! `internalOperatorMemberThroughFactoryYieldsToExtension.kt` reaches the same calls through a
//! library function.
use super::common;

const PROGRAM: &str =
    include_str!("box_regressions/multimodule/internalOperatorMemberYieldsToExtension.kt");

#[test]
fn an_internal_member_of_a_constructed_dependency_class_yields_to_extensions_like_kotlinc() {
    let modules =
        krusty::conformance::split_modules(PROGRAM).expect("the program is a // MODULE: graph");
    let [lib, main] = modules.as_slice() else {
        panic!("the program declares a library module and a main module");
    };
    assert_eq!(
        main.deps,
        ["lib"],
        "the main module depends on the library module"
    );
    let [(_, lib_source)] = lib.files.as_slice() else {
        panic!("the library module is one file");
    };
    let [(_, main_source)] = main.files.as_slice() else {
        panic!("the main module is one file");
    };
    let library = common::kotlinc_library(lib_source).expect("kotlinc builds the library module");
    let reference =
        common::kotlinc_box_result_with_classpath(main_source, std::slice::from_ref(&library));
    assert_eq!(reference, "OK", "kotlinc runs the original program");
    assert_eq!(
        common::expect_box_run_against("internal_operator_member", lib_source, main_source)
            .as_deref(),
        Some(reference.as_str()),
    );
}
