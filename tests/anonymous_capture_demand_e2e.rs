//! Exact JVM artifacts for the shared anonymous-delegate lifetime program.

// PLATFORM-SPECIFIC: compare the complete JVM ABI and raw/decoded Kotlin metadata.
// The portable whole source also runs through the canonical backend/version inventory.
use super::common;

#[test]
fn nested_interface_delegate_operands_keep_their_construction_lifetimes() {
    let source = include_str!("box_regressions/interfaceDelegateCaptureLifetimes.kt");
    common::expect_box_same_as_kotlinc(source, "interfaceDelegateCaptureLifetimes");
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "interfaceDelegateCaptureLifetimes.kt",
        source,
    )]));
}

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
fn enum_entry_property_receivers_keep_their_owner_across_spilled_writes() {
    let source = include_str!("box_regressions/enumEntryPropertyReceiverWrites.kt");
    common::expect_box_same_as_kotlinc(source, "enumEntryPropertyReceiverWrites");
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "enumEntryPropertyReceiverWrites.kt",
        source,
    )]));
}

#[test]
fn an_enum_entry_property_keeps_its_owner_through_an_anonymous_inner_class() {
    let source = include_str!("box_regressions/deepInnerClassInEnumEntryClass2.kt");
    common::expect_box_same_as_kotlinc(source, "deepInnerClassInEnumEntryClass2");
    assert_complete_abi_and_metadata(&common::classes_against_kotlinc_module(&[(
        "deepInnerClassInEnumEntryClass2.kt",
        source,
    )]));
}
