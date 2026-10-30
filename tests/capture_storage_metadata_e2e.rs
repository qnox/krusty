//! Capture storage is physical state, not a Kotlin property declaration.

// PLATFORM: complete JVM class ABI, metadata payloads and runtime against kotlinc.
use super::common;

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


fn assert_capture_metadata(stem: &str, source: &str) {
    let file = format!("{stem}.kt");
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(&file, source)]));
    common::expect_box_same_as_kotlinc(source, stem);
}

#[test]
fn retained_inline_capture_storage_is_not_a_property() {
    assert_capture_metadata("InlineCaptureMetadata", r#"
interface Token { fun value(): Int }
private inline fun copy(value: Int) = object : Token { override fun value(): Int = value }
fun box(): String = if (copy(7).value() == 7) "OK" else "capture"
"#);
}

#[test]
fn a_real_property_can_share_a_captured_values_spelling() {
    assert_capture_metadata("InlineCapturePropertyMetadata", r#"
interface Token { fun value(): Int }
private inline fun copy(value: Int) = object : Token {
    val value: Int = value
    override fun value(): Int = this.value
}
fun box(): String = if (copy(7).value() == 7) "OK" else "property"
"#);
}

#[test]
fn retained_default_capture_storage_is_not_a_property() {
    assert_capture_metadata("DefaultCaptureMetadata", r#"
interface Token { fun value(): Int }
private fun copy(value: Int, token: Token = object : Token { override fun value(): Int = value }): Token = token
fun box(): String = if (copy(7).value() == 7) "OK" else "default"
"#);
}

#[test]
fn ordinary_capture_storage_keeps_its_metadata_and_runtime() {
    assert_capture_metadata("OrdinaryCaptureMetadata", r#"
interface Token { fun value(): Int }
private fun copy(value: Int) = object : Token { override fun value(): Int = value }
fun box(): String = if (copy(7).value() == 7) "OK" else "ordinary"
"#);
}

