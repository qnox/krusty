//! Bare-call lookup retains the default-imported function's contract past an extension-only
//! scope level. Both complete lookup regressions also run through the shared portable inventory.
use super::common;

#[test]
fn a_bare_call_reaches_the_default_import_past_a_same_package_extension() {
    let (sources, java) = krusty::conformance::split_files(include_str!(
        "../box_regressions/bareCallPastReceiverOnlyLevelContracts.kt"
    ))
    .expect("partition");
    assert!(java.is_empty());
    let [(stem, program)] = sources.as_slice() else {
        panic!("the complete bare-call contract regression")
    };
    assert_eq!(stem, "BareCallPastReceiverOnlyLevel");
    common::expect_box_same_as_kotlinc(program, "BareCallPastReceiverOnlyLevel");
}
