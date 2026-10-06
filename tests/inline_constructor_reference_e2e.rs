//! `values.map(::Holder)` is the stdlib collection transform with the constructor call spliced
//! in, not a function-reference object.

use super::common;

const SRC: &str = "\
class Holder(val name: String)\n\
fun names(values: List<String>): List<Holder> = values.map(::Holder)\n\
";

#[test]
fn an_inline_map_of_a_constructor_reference_matches_kotlinc() {
    let difference = common::method_code_diff_against_kotlinc(
        "InlineCtorRef",
        &[],
        SRC,
        "InlineCtorRefKt",
        "public static final java.util.List<Holder> names(",
    )
    .expect("reference kotlinc is provisioned");
    assert_eq!(difference, Ok(()));
}
