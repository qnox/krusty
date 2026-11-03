//! `Sequence.filterIsInstance<R>()` is a public reified classpath function whose body calls
//! `Intrinsics.needClassReification` and then loads the singleton
//! `SequencesKt___SequencesKt$filterIsInstance$1`. That singleton's `invoke` is
//! `reifiedOperationMarker` plus `instanceof Object`. kotlinc copies the class for the call site,
//! specializes the marker, and deletes `needClassReification`. A direct call throws.

use super::common;

const NAMES: &str = r#"
fun Sequence<*>.names(): Sequence<String> = filterIsInstance<String>()

fun box(): String {
    val found = sequenceOf<Any>("a", 1, "b").names().toList()
    if (found != listOf("a", "b")) return "FAIL $found"
    return "OK"
}
"#;

const PROPERTIES: &str = r#"
fun names(props: java.util.Properties): String? =
    props.propertyNames().asSequence()
        .filterIsInstance<String>()
        .filter { it.contains('$') }
        .firstOrNull()

fun box(): String {
    val props = java.util.Properties()
    props.setProperty("plain", "1")
    props.setProperty("\$keep", "2")
    val found = names(props)
    if (found != "\$keep") return "FAIL $found"
    return "OK"
}
"#;

#[test]
fn sequence_filter_is_instance_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(NAMES, "reified_sequence_filter");
    common::expect_box_same_as_kotlinc(PROPERTIES, "reified_sequence_filter_properties");
}

/// The copied singleton and the caller that loads it are kotlinc's classes.
#[test]
fn sequence_filter_is_instance_is_the_reference_compilers_class() {
    let classes = common::classes_against_kotlinc_lib(
        "Seq",
        &[("Lib.kt", "package lib\nfun unused(): Int = 1\n")],
        "fun names(seq: Sequence<*>): Sequence<String> = seq.filterIsInstance<String>()\n",
    )
    .expect("reference kotlinc is provisioned");
    assert!(
        classes
            .reference
            .keys()
            .any(|name| name.contains("$$inlined$filterIsInstance$")),
        "kotlinc did not regenerate filterIsInstance: {:?}",
        classes.reference.keys().collect::<Vec<_>>()
    );
    let differences = classes.differences();
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}

/// A library class that declares an inline function with a reified parameter has
/// `reifiedOperationMarker` in its methods, but it is no object a call regenerates: the
/// `needClassReification` before the definition lambda asks only that lambda to be specialized.
/// `singleOf`'s inlined body also names `Module`, which stays as it is, as in kotlinc.
const MODULE_LIB: &str = r#"package lib

class Module {
    val definitions = mutableListOf<Pair<String, () -> Any?>>()

    inline fun <reified T> single(noinline definition: () -> T) {
        definitions += (T::class.simpleName ?: "?") to definition
    }
}

inline fun <reified R> Module.singleOf(crossinline make: () -> R) =
    single { make().also { check(it is R) } }
"#;

const MODULE_MAIN: &str = r#"import lib.*

fun box(): String {
    val module = Module()
    module.singleOf { "OK" }
    val (name, definition) = module.definitions.single()
    return if (name == "String") definition() as String else name
}
"#;

#[test]
fn a_reified_lambda_beside_a_class_with_reified_members_is_the_reference_compilers_class() {
    let classes =
        common::classes_against_kotlinc_lib("Main", &[("Lib.kt", MODULE_LIB)], MODULE_MAIN)
            .expect("reference kotlinc is provisioned");
    assert_eq!(
        classes.reference.keys().collect::<Vec<_>>(),
        ["MainKt", "MainKt$box$$inlined$singleOf$1"]
    );
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn a_reified_lambda_beside_a_class_with_reified_members_runs() {
    let output = common::expect_box_run_against_kotlinc(MODULE_LIB, MODULE_MAIN)
        .expect("reference kotlinc is provisioned");
    assert_eq!(output, "OK");
}
