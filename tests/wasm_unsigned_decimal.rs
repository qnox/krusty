use super::*;

#[test]
fn unsigned_decimal_strings_use_real_klib_declarations_on_both_wasm_targets() {
    for (source, stem) in [
        (
            include_str!("box_regressions/unsignedDecimalStrings.kt"),
            "unsignedDecimalStrings",
        ),
        (
            include_str!("box_regressions/nodeKindLineRule.kt"),
            "nodeKindLineRule",
        ),
    ] {
        super::super::box_reference_classes::checked_box(source, stem)
            .expect("the exact Kotlin compiler must run the complete decimal fixture");
        expect_wasm_box(source, stem);
    }
}
