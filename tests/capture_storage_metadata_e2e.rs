//! Capture storage is physical state, not a Kotlin property declaration.

// PLATFORM: complete JVM class ABI, metadata payloads and runtime against kotlinc.
// PLATFORM-SPECIFIC: compare complete JVM ABI and raw/decoded Kotlin metadata.
// Whole Kotlin inputs also run through the shared portable backend/version inventory.
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
    assert_capture_metadata(
        "InlineCaptureMetadata",
        include_str!("box_regressions/captureStorageInlineMetadata.kt"),
    );
}

#[test]
fn a_real_property_can_share_a_captured_values_spelling() {
    assert_capture_metadata(
        "InlineCapturePropertyMetadata",
        include_str!("box_regressions/captureStorageRealPropertyMetadata.kt"),
    );
}

#[test]
fn retained_default_capture_storage_is_not_a_property() {
    assert_capture_metadata(
        "DefaultCaptureMetadata",
        include_str!("box_regressions/captureStorageDefaultMetadata.kt"),
    );
}

#[test]
fn ordinary_capture_storage_keeps_its_metadata_and_runtime() {
    assert_capture_metadata(
        "OrdinaryCaptureMetadata",
        include_str!("box_regressions/captureStorageOrdinaryMetadata.kt"),
    );
}

#[test]
fn a_super_argument_capture_uses_its_parameter_without_an_extra_field() {
    assert_capture_metadata(
        "SuperArgumentCaptureMetadata",
        include_str!("box_regressions/captureStorageSuperArgumentMetadata.kt"),
    );
}

#[test]
fn member_parameters_do_not_capture_shadowed_enclosing_values() {
    assert_capture_metadata(
        "ShadowedCaptureMetadata",
        include_str!("box_regressions/captureStorageShadowedMetadata.kt"),
    );
}

#[test]
fn forwarded_super_arguments_keep_the_enclosing_capture_owner() {
    assert_capture_metadata(
        "ForwardedCaptureOwnerMetadata",
        include_str!("box_regressions/captureStorageForwardedOwnerMetadata.kt"),
    );
}

#[test]
fn a_receiver_function_in_an_initializer_reads_its_constructor_parameter() {
    assert_capture_metadata(
        "InitializerFunctionCaptureMetadata",
        include_str!("box_regressions/captureStorageInitializerFunctionMetadata.kt"),
    );
}

#[test]
fn a_selected_local_function_keeps_a_capture_shadowed_at_its_call_site() {
    assert_capture_metadata(
        "TransitiveShadowedCaptureMetadata",
        include_str!("box_regressions/captureStorageTransitiveShadowedMetadata.kt"),
    );
}

#[test]
fn a_parameter_in_one_member_does_not_hide_another_members_capture() {
    assert_capture_metadata(
        "IndependentMemberCaptureMetadata",
        include_str!("box_regressions/captureStorageIndependentMemberMetadata.kt"),
    );
}

#[test]
fn a_receiver_function_parameter_hides_the_enclosing_function_value() {
    assert_capture_metadata(
        "ShadowedFunctionCaptureMetadata",
        include_str!("box_regressions/captureStorageShadowedFunctionMetadata.kt"),
    );
}

#[test]
fn a_nested_super_argument_reads_the_enclosing_capture_coordinate() {
    assert_capture_metadata(
        "NestedSuperCaptureCoordinateMetadata",
        include_str!("box_regressions/captureStorageNestedSuperMetadata.kt"),
    );
}
