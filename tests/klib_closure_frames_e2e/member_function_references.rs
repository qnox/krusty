//! PLATFORM: official-producer member function references.

use super::{assert_official_dependency_sources, common, DependencyConsumer};

/// A library body references a member function unbound and bound, on the class and on its
/// subclass: each invocation dispatches on the receiver the reference was given or captured.
#[test]
fn dependency_member_references_dispatch_on_their_receiver() {
    if std::env::var_os("KRUSTY_KOTLIN_NATIVE").is_none() {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    }
    let modules = krusty::conformance::split_modules(include_str!(
        "../box_regressions/multimodule/dependencyMemberFunctionReferences.kt"
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
    assert_official_dependency_sources(
        library,
        &[(stem.as_str(), consumer_text.as_str())],
        "DependencyMemberFunctionReferences",
        &DependencyConsumer::Native,
    );
}
