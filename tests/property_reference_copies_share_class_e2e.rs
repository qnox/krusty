//! A lambda keeps a value-producing copy of its body for call sites that splice it, so a property
//! reference written in the lambda is two nodes that share the name the local-class walk gave the
//! source reference. kotlinc writes one class per reference written in the source: krusty used to
//! add a `$fir$property$N` class for the copy.

use super::common;

const PROGRAM: &str = include_str!("box_regressions/propertyReferenceInLambdaCopies.kt");
const STEM: &str = "PropertyReferenceInLambdaCopies";

#[test]
fn copies_of_a_source_property_reference_are_one_class() {
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
            "Holder",
            "PropertyReferenceInLambdaCopiesKt",
            "PropertyReferenceInLambdaCopiesKt$box$read$1$1",
        ]
    );
    for class in &names {
        common::byte_diff_against_kotlinc_cp(STEM, PROGRAM, class, &[common::stdlib_jar()])
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
