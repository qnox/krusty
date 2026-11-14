//! A primitive value reaches a reference supertype through its Kotlin classifier's applied
//! hierarchy, type arguments included. The whole program is the shared box regression, so every
//! backend lane runs it. This control asks kotlinc before the JVM and Native results.

use super::common;

#[test]
fn a_primitive_reaches_the_reference_supertypes_its_classifier_declares() {
    let source = include_str!("box_regressions/primitiveReferenceSupertypes.kt");
    assert_eq!(common::kotlinc_box_result(source), "OK");
    common::expect_box_ok_with_stdlib(source, "PrimitiveReferenceSupertypes");
    common::expect_native_box(source, "PrimitiveReferenceSupertypes", "OK");
}

#[test]
fn a_primitive_does_not_reach_a_supertype_application_it_does_not_implement() {
    const SRC: &str = "fun invalid(value: Int): Comparable<String> = value\n\
fun valid(value: Int): Comparable<Int> = value\n";
    let sources = [("Main.kt", SRC)];
    let reference_args = common::language_directives::kotlinc_args(SRC);
    assert_eq!(
        common::reference_error_blocks(&sources, &reference_args),
        ["Main.kt:1:47: return type mismatch: expected 'Comparable<String>', actual 'Int'."]
    );
    common::assert_error_blocks_match_kotlinc(&sources, &reference_args);
}
