//! File-facade metadata retains declaration-owned local classifier facts.

// PLATFORM: complete JVM metadata payloads and class ABI against the reference compiler.
use super::common;

fn assert_facade_metadata(stem: &str, source: &str) {
    let file = format!("{stem}.kt");
    let classes = common::classes_against_kotlinc_module(&[(&file, source)]);
    let name = format!("{stem}Kt");
    let metadata = |bytes: &Vec<u8>| {
        let info = krusty::jvm::classreader::parse_class(bytes).expect("readable class");
        (
            format!("{:?}", info.meta),
            common::kotlin_metadata::kotlin_metadata_ints(bytes),
            common::kotlin_metadata::kotlin_metadata_d1_parts(bytes),
            common::raw_kotlin_metadata(bytes),
        )
    };
    assert_eq!(
        metadata(&classes.krusty[&name]),
        metadata(&classes.reference[&name])
    );
}

#[test]
fn private_nongeneric_anonymous_factory_metadata_matches_kotlinc() {
    assert_facade_metadata(
        "NongenericAnonymousMetadataBaseline",
        r#"
interface Token<T>
private fun copy() = object : Token<Number> {}
fun box(): String = "OK"
"#,
    );
}

#[test]
fn explicitly_typed_getter_of_anonymous_factory_metadata_matches_kotlinc() {
    assert_facade_metadata(
        "ExplicitGetterMetadataBaseline",
        r#"
interface Token<T>
private fun copy() = object : Token<Number> {}
val callback: () -> Token<Number> get() = { copy() }
fun box(): String = "OK"
"#,
    );
}

const ANONYMOUS_RESULT_GETTERS: &str = r#"
interface Token<T>
private fun copy() = object : Token<Number> {}
val callback get() = { copy() }
class GetterOwner {
    val callback get() = { copy() }
    fun callback() = { copy() }
}
fun box(): String {
    callback()
    GetterOwner().callback()
    GetterOwner().callback()()
    return "OK"
}
"#;

fn assert_complete_abi_and_metadata(classes: &common::ClassSets) {
    let inventory = |classes: &std::collections::BTreeMap<String, Vec<u8>>| {
        classes
            .iter()
            .map(|(name, bytes)| {
                let info = krusty::jvm::classreader::parse_class(bytes).expect("readable class");
                let header = format!(
                    "{:?}",
                    (
                        info.access,
                        info.this_class,
                        info.super_class,
                        info.interfaces(),
                        info.signature,
                        info.inner_classes,
                    )
                );
                (
                    name.clone(),
                    (
                        header,
                        common::member_table(bytes),
                        (
                            format!("{:?}", info.meta),
                            common::kotlin_metadata::kotlin_metadata_elements(bytes),
                        ),
                    ),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let actual = inventory(&classes.krusty);
    let expected = inventory(&classes.reference);
    let actual_abi = actual
        .iter()
        .map(|(name, (header, members, _))| (name.clone(), (header.clone(), members.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    let expected_abi = expected
        .iter()
        .map(|(name, (header, members, _))| (name.clone(), (header.clone(), members.clone())))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(actual_abi, expected_abi, "complete class ABI");
    assert_eq!(actual, expected, "complete class metadata");
}

#[test]
fn anonymous_factory_inferred_getters_preserve_complete_class_abi_and_metadata() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "AnonymousResultGetters.kt",
        ANONYMOUS_RESULT_GETTERS,
    )]));
}

#[test]
fn anonymous_property_storage_preserves_complete_class_abi_and_metadata() {
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "AnonymousPropertyStorage.kt",
        r#"
interface Token<T>
private val stored = object : Token<Number> { val value = "OK" }
fun box(): String = stored.value
"#,
    )]));
}
