//! A lambda keeps a value-producing copy of its body for call sites that splice it, so a callable
//! reference written in the lambda exists as two nodes that share one adapter and the name the
//! local-class walk gave the source reference. kotlinc writes exactly one class per reference
//! written in the source: krusty used to add a `$fir$function$N` class for each copy, and the
//! adapter, now shared by two references, stayed a facade static every carrier called.

use super::common;

const PROGRAM: &str =
    include_str!("box_regressions/callableReferenceToImplicitReceiverUnitCoercion.kt");
const STEM: &str = "CallableReferenceToImplicitReceiverUnitCoercion";

#[test]
fn copies_of_a_source_reference_are_one_class() {
    assert_eq!(common::expect_box_run_with_stdlib(PROGRAM, STEM), "OK");
    let classes = common::compile_in_process_metadata_cp_module_target(
        PROGRAM,
        STEM,
        &[common::stdlib_jar()],
        "main",
        None,
    )
    .expect("the program compiles");
    let mut names = classes
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "CallableReferenceToImplicitReceiverUnitCoercionKt",
            "CallableReferenceToImplicitReceiverUnitCoercionKt$box$1$1",
            "CallableReferenceToImplicitReceiverUnitCoercionKt$box$built$1$1",
            "Items",
            "Source",
        ]
    );
    for class in [
        "CallableReferenceToImplicitReceiverUnitCoercionKt$box$1$1",
        "CallableReferenceToImplicitReceiverUnitCoercionKt$box$built$1$1",
    ] {
        common::byte_diff_against_kotlinc_cp(STEM, PROGRAM, class, &[common::stdlib_jar()])
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

const LOCAL_PROGRAM: &str =
    include_str!("box_regressions/localFunctionReferenceInInlineLambdaCopies.kt");
const LOCAL_STEM: &str = "LocalFunctionReferenceInInlineLambdaCopies";

/// The first copy's own `invoke` takes the shared adapter apart (it becomes an instance method),
/// so a later copy instantiates the class without reading the adapter again.
#[test]
fn a_copy_of_a_local_function_reference_reads_nothing_from_the_adapter() {
    assert_eq!(
        common::expect_box_run_with_stdlib(LOCAL_PROGRAM, LOCAL_STEM),
        "OK"
    );
    let classes = common::compile_in_process_metadata_cp_module_target(
        LOCAL_PROGRAM,
        LOCAL_STEM,
        &[common::stdlib_jar()],
        "main",
        None,
    )
    .expect("the program compiles");
    let mut names = classes
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "LocalFunctionReferenceInInlineLambdaCopiesKt",
            "LocalFunctionReferenceInInlineLambdaCopiesKt$test$1$1",
        ]
    );
    let class = "LocalFunctionReferenceInInlineLambdaCopiesKt$test$1$1";
    common::byte_diff_against_kotlinc_cp(LOCAL_STEM, LOCAL_PROGRAM, class, &[common::stdlib_jar()])
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
}
