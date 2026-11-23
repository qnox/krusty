//! PLATFORM: official-producer classes whose constructors declare parameter defaults.

use super::{assert_official_dependency_sources, common, DependencyConsumer};

/// Constructions and delegations that leave arguments out reach the constructor's
/// default-argument entry, which runs the defaults the library declares.
#[test]
fn dependency_class_constructors_fill_their_parameter_defaults() {
    if std::env::var_os("KRUSTY_KOTLIN_NATIVE").is_none() {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    }
    let modules = krusty::conformance::split_modules(include_str!(
        "../box_regressions/multimodule/dependencyConstructorDefaults.kt"
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
        "DependencyConstructorDefaults",
        &DependencyConsumer::Native,
    );
}
