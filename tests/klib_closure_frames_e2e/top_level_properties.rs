//! PLATFORM: official-producer top-level properties and Native dependency execution.

use super::{
    assert_official_dependency_closures, assert_official_dependency_sources, common,
    DependencyConsumer,
};

#[test]
fn dependency_top_level_properties_share_one_storage_location() {
    assert_official_dependency_closures(
        include_str!("../fixtures/klib_closure_frames/TopLevelProperties.kt"),
        include_str!("../fixtures/klib_closure_frames/TopLevelPropertyConsumer.kt"),
        "DependencyTopLevelProperties",
        DependencyConsumer::Native,
    );
}

#[test]
fn dependency_file_private_properties_run_their_file_initialization_once() {
    if std::env::var_os("KRUSTY_KOTLIN_NATIVE").is_none() {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    }
    let modules = krusty::conformance::split_modules(include_str!(
        "../box_regressions/multimodule/dependencyFilePrivateInitialization.kt"
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
        "DependencyFilePrivateProperties",
        &DependencyConsumer::Native,
    );
}
