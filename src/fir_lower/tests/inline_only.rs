use super::*;

#[test]
fn source_inline_only_annotation_marks_its_function_as_a_non_emitted_template() {
    let ir = lower_single_source_with_jvm_stdlib(
        "@Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n\
         @kotlin.internal.InlineOnly\n\
         inline fun <T, R> T.through(block: (T) -> R): R = block(this)\n\
         fun use(): String = \"OK\".through { it }\n",
        "SourceInlineOnly",
    );
    let function = ir
        .functions
        .iter()
        .enumerate()
        .find_map(|(function, declaration)| {
            (declaration.name == "through").then_some(function as crate::ir::FunId)
        })
        .expect("the inline-only source function is declared");
    assert!(ir.inline_fns.contains(&function));
    assert!(ir.inline_only_fns.contains(&function));
}
