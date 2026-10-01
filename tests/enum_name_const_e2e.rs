//! `Enum.name` on a direct entry is that entry's declaration name. The read does not initialize
//! the enum. A name read through a value still calls `name()` and does run the initializer.

use super::common;

#[test]
fn a_direct_entry_name_does_not_initialize_the_enum() {
    const SRC: &str = r#"
// LANGUAGE: +IntrinsicConstEvaluation
enum class EnumClass { OK; init { result = "Fail" } }
var result = "OK"
fun <T> T.id() = this
fun box(): String {
    if (EnumClass.OK.name.id() != "OK") return "Fail"
    return result
}
"#;
    common::expect_box_same_as_kotlinc(SRC, "EnumNameDirectEntry");
}

#[test]
fn each_direct_entry_name_is_its_declaration_name() {
    const SRC: &str = r#"
// LANGUAGE: +IntrinsicConstEvaluation
enum class Side { LEFT, RIGHT; init { touched = "Fail" } }
var touched = "OK"
fun box(): String {
    if (Side.LEFT.name != "LEFT") return "Fail left"
    if (Side.RIGHT.name != "RIGHT") return "Fail right"
    return touched
}
"#;
    common::expect_box_same_as_kotlinc(SRC, "EnumNameEachEntry");
}

#[test]
fn a_name_read_through_a_value_still_initializes_the_enum() {
    const SRC: &str = r#"
enum class EnumClass { OK; init { touched = "ran" } }
var touched = "idle"
fun box(): String {
    val entry: EnumClass = EnumClass.OK
    if (entry.name != "OK") return "Fail name"
    if (touched != "ran") return "Fail init"
    return "OK"
}
"#;
    common::expect_box_same_as_kotlinc(SRC, "EnumNameThroughValue");
}

#[test]
fn direct_and_through_value_reads_match_kotlincs_exact_instructions() {
    const SRC: &str = r#"
// LANGUAGE: +IntrinsicConstEvaluation
enum class Direction { LEFT, RIGHT }
fun direct(): String = Direction.LEFT.name
fun through(value: Direction): String = value.name
"#;
    let options = common::language_directives::kotlinc_args(SRC);
    let built = common::compare_with_kotlinc_plugin(
        "EnumNameInstructions",
        SRC,
        "EnumNameInstructionsKt",
        &[common::stdlib_jar()],
        "25",
        &options,
    )
    .expect("reference kotlinc and javap are provisioned");
    for member in [
        "java.lang.String direct();",
        "java.lang.String through(Direction);",
    ] {
        let reference = common::method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "kotlinc emits {member}");
        assert_eq!(
            common::method_instructions(&built.krusty, member),
            reference,
            "exact instructions for {member}"
        );
    }
}
