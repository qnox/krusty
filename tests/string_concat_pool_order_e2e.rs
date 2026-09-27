//! From JVM 9 kotlinc compiles a string template to one `invokedynamic makeConcatWithConstants`.
//! Its writer interns constants in instruction order, so the operands' entries come first and the
//! recipe, the `StringConcatFactory` bootstrap handle and the call site follow them.
use super::common;

#[test]
fn a_template_interns_its_operands_before_its_concat_call_site() {
    let source = "class Holder(val size: Int, val name: String)\n\
                  fun describe(holder: Holder): String = \"size=${holder.size} name=${holder.name}\"\n";
    let compared = common::compile_with_kotlinc_for_target("Concat", source, 11, &["ConcatKt"]);
    let (expected, actual) = &compared[0];
    assert!(actual == expected, "ConcatKt differs from kotlinc");
}
