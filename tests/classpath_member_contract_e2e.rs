//! A `callsInPlace` contract declared by a class or object member of a compiled dependency is
//! applied at the call site like one declared by a top-level function: a deferred local assigned
//! inside the in-place lambda is definitely initialized afterwards. The dependency is built by the
//! reference kotlinc, as a real dependency jar's class files are, and the consumer runs to `OK`
//! under both compilers. The whole library and consumer programs live in the shared portable
//! inventory.

use super::common;

#[test]
fn a_dependency_members_calls_in_place_contract_initializes_deferred_locals() {
    let modules = krusty::conformance::split_modules(include_str!(
        "box_regressions/multimodule/dependencyMemberContractInitialization.kt"
    ))
    .expect("the whole contract fixture has a library and a main module");
    let [library, main] = modules.as_slice() else {
        panic!("expected the library and main modules");
    };
    assert_eq!((library.files.len(), main.files.len()), (1, 1));
    let (library_name, library_source) = &library.files[0];
    let (_, main_source) = &main.files[0];
    assert_eq!(library_name, "Lib");
    let library_classes = common::kotlinc_lib_out(&[("Lib.kt", library_source)])
        .expect("reference kotlinc builds the contract dependency");
    assert_eq!(
        common::kotlinc_box_result_with_classpath(
            main_source,
            std::slice::from_ref(&library_classes)
        ),
        "OK",
        "kotlinc's own result"
    );
    assert_eq!(
        common::expect_box_run_against_kotlinc(library_source, main_source)
            .expect("the reference compiler builds the dependency"),
        "OK",
        "krusty's result against the same dependency"
    );
}
