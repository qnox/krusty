//! A suspend conversion of a function value names the class its operand declares after its own:
//! the property reference in `::a.get()` is `box$1$1`, inside the conversion class `box$1`.

use super::common;

const PROGRAM: &str = include_str!("box_regressions/functionValueConversionOperandReference.kt");
const STEM: &str = "FunctionValueConversionOperandReference";

#[test]
fn a_reference_in_a_converted_operand_is_named_inside_the_conversion() {
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
            "FunctionValueConversionOperandReferenceKt",
            "FunctionValueConversionOperandReferenceKt$box$1",
            "FunctionValueConversionOperandReferenceKt$box$1$1",
            "FunctionValueConversionOperandReferenceKt$runSuspend$$inlined$Continuation$1",
        ]
    );
    for class in names
        .iter()
        .filter(|class| class.ends_with("$box$1") || class.ends_with("$box$1$1"))
    {
        common::byte_diff_against_kotlinc_cp(STEM, PROGRAM, class, &[common::stdlib_jar()])
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}
