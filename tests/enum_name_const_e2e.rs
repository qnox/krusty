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

/// Under `+IntrinsicConstEvaluation` a direct entry's `name` is a constant in an annotation
/// argument, alone, as a vararg element and in an array, and in a `const val` initializer.
#[test]
fn a_direct_entry_name_is_an_annotation_and_const_val_constant() {
    const SRC: &str = r#"
// LANGUAGE: +IntrinsicConstEvaluation
@Retention(AnnotationRetention.RUNTIME)
annotation class Name(val value: String)
@Retention(AnnotationRetention.RUNTIME)
annotation class Names(vararg val values: String)
@Retention(AnnotationRetention.RUNTIME)
annotation class NameArray(val values: Array<String>)
enum class E { OK, WITH_UNDERSCORE }
const val NAME = E.OK.name
@Name(E.OK.name) class Single
@Names(E.WITH_UNDERSCORE.name, "x" + E.OK.name, "${E.OK.name}!") class Vararg
@NameArray([E.OK.name, NAME]) class InArray
fun box(): String {
    val single = Single::class.java.getAnnotation(Name::class.java).value
    val vararg = Vararg::class.java.getAnnotation(Names::class.java).values.joinToString()
    val array = InArray::class.java.getAnnotation(NameArray::class.java).values.joinToString()
    if (vararg != "WITH_UNDERSCORE, xOK, OK!") return "vararg $vararg"
    if (array != "OK, OK") return "array $array"
    return if (single == NAME) single else "single $single"
}
"#;
    common::expect_box_same_as_kotlinc(SRC, "EnumNameAnnotationConstant");
}
