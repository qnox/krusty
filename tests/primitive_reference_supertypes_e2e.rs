//! A primitive value reaches a reference supertype through its Kotlin classifier's applied
//! hierarchy, type arguments included, against kotlinc. The positive forms are the shared box
//! regression `tests/box_regressions/primitiveReferenceSupertypes.kt`.

use super::common;

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
