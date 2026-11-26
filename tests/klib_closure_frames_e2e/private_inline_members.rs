//! PLATFORM: private inline members of an official-producer class, called from its public members.

use super::{assert_official_dependency_sources_produced_with, common, DependencyConsumer};

/// A 2.4 producer inlines a library's own private inline calls in its first stage; a library built
/// without that stage, as a release of an earlier compiler is, keeps them as calls its consumer
/// expands.
#[test]
fn dependency_private_inline_members_expand_into_their_class_members() {
    if std::env::var_os("KRUSTY_KOTLIN_NATIVE").is_none() {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    }
    let modules = krusty::conformance::split_modules(include_str!(
        "../box_regressions/multimodule/dependencyPrivateInlineMembers.kt"
    ))
    .expect("the complete two-module regression");
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
    assert_official_dependency_sources_produced_with(
        library,
        &["-Xklib-ir-inliner=disabled"],
        &[(stem.as_str(), consumer_text.as_str())],
        "DependencyPrivateInlineMembers",
        &DependencyConsumer::Native,
    );
}
