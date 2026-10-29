//! Annotation constructor defaults retain their complete metadata value.

// PLATFORM: complete JVM artifacts and decoded/raw Kotlin metadata against kotlinc.
use super::{common, NESTED_CLASS_ARGUMENTS};

#[test]
fn the_original_nested_annotation_fixture_matches_every_class_artifact() {
    let classes =
        common::classes_against_kotlinc_module(&[("Declarations.kt", NESTED_CLASS_ARGUMENTS)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn the_original_nested_annotation_fixture_matches_complete_metadata() {
    let classes =
        common::classes_against_kotlinc_module(&[("Declarations.kt", NESTED_CLASS_ARGUMENTS)]);
    let inventory = |classes: &std::collections::BTreeMap<String, Vec<u8>>| {
        classes
            .iter()
            .map(|(name, bytes)| {
                let info = krusty::jvm::classreader::parse_class(bytes).expect("readable class");
                (
                    name.clone(),
                    (
                        format!("{:?}", info.meta),
                        common::kotlin_metadata::kotlin_metadata_elements(bytes),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(inventory(&classes.krusty), inventory(&classes.reference));
}

const DEFAULT_KINDS: &str = r#"
import kotlin.reflect.KClass
enum class Choice { FIRST, SECOND }
class Host { class Nested }
annotation class Leaf(val value: Int = 3)
annotation class Defaults(
    val flag: Boolean = true,
    val byte: Byte = 1,
    val short: Short = 2,
    val char: Char = 'x',
    val int: Int = 3,
    val long: Long = 4L,
    val float: Float = 1.25f,
    val double: Double = 2.5,
    val text: String = "value",
    val choice: Choice = Choice.SECOND,
    val type: KClass<*> = Host.Nested::class,
    val annotation: Leaf = Leaf(7),
    val empty: IntArray = [],
    val ints: IntArray = [1, 2],
    val types: Array<KClass<*>> = [String::class, Host.Nested::class]
)
annotation class Required(val value: Int)
class Ordinary(val value: Int = 3)
fun ordinary(value: String = "value"): String = value
"#;

#[test]
fn annotation_default_kinds_and_ordinary_defaults_match_complete_artifacts() {
    let classes = common::classes_against_kotlinc_module(&[("DefaultKinds.kt", DEFAULT_KINDS)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

const ANNOTATED_PARAMETERS: &str = r#"
import kotlin.reflect.KClass
@Target(AnnotationTarget.VALUE_PARAMETER)
annotation class Mark(val label: String)
class Host { class Nested }
annotation class Ordered(
    @param:Mark("first") val type: KClass<*> = Host.Nested::class,
    @param:Mark("second") val values: IntArray = [3, 4]
)
"#;

#[test]
fn annotation_defaults_precede_parameter_annotations_in_metadata_order() {
    let classes = common::classes_against_kotlinc_module(&[("Ordered.kt", ANNOTATED_PARAMETERS)]);
    assert_eq!(classes.differences(), Vec::<String>::new());
}
