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
