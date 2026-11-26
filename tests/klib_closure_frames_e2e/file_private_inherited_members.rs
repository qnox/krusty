//! PLATFORM: official-producer calls of members a file-private class inherits.

use super::{assert_official_dependency_sources, common, DependencyConsumer};

#[test]
fn dependency_file_private_classes_call_the_members_they_inherit() {
    assert_file_private_inheritance(
        include_str!("../box_regressions/multimodule/dependencyFilePrivateInheritedMembers.kt"),
        "DependencyFilePrivateInheritedMembers",
    );
}

/// A file-private class carries the interface defaults it inherits, whether its own member calls
/// one or only dispatch through the interface reaches it.
#[test]
fn dependency_file_private_classes_implement_inherited_interface_defaults() {
    assert_file_private_inheritance(
        include_str!("../box_regressions/multimodule/dependencyFilePrivateInterfaceDefaults.kt"),
        "DependencyFilePrivateInterfaceDefaults",
    );
}

fn assert_file_private_inheritance(source: &str, fixture_name: &str) {
    if std::env::var_os("KRUSTY_KOTLIN_NATIVE").is_none() {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    }
    let modules =
        krusty::conformance::split_modules(source).expect("the complete two-module regression");
    let [producer, consumer] = modules.as_slice() else {
        panic!("producer and consumer modules")
    };
    assert_eq!(consumer.deps, std::slice::from_ref(&producer.name));
    let ([(_, library)], [(stem, consumer_text)]) =
        (producer.files.as_slice(), consumer.files.as_slice())
    else {
        panic!("one complete producer file and one consumer file")
    };
    let dependency = common::kotlinc_lib_out(&[("Library.kt", library)])
        .expect("reference compiler required for the dependency consumer oracle");
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath(
            &[("Consumer.kt", consumer_text)],
            "fixture.ConsumerKt",
            &[dependency],
        ),
        "OK"
    );
    assert_official_dependency_sources(
        library,
        &[(stem.as_str(), consumer_text.as_str())],
        fixture_name,
        &DependencyConsumer::Native,
    );
}
