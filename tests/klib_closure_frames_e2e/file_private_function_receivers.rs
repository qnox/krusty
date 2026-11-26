//! PLATFORM: an official-producer dependency body that passes a function value to a parameter
//! whose receiver is a file-private class of the same library.

use super::{assert_official_dependency_sources, common, DependencyConsumer};

#[test]
fn a_function_of_a_supertype_receiver_is_passed_where_its_file_private_subclass_is_the_receiver() {
    if std::env::var_os("KRUSTY_KOTLIN_NATIVE").is_none() {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    }
    let modules = krusty::conformance::split_modules(include_str!(
        "../box_regressions/multimodule/dependencyFilePrivateClassAsFunctionReceiver.kt"
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
        "DependencyFilePrivateClassAsFunctionReceiver",
        &DependencyConsumer::Native,
    );
}
