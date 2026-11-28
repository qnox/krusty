//! PLATFORM: official-producer bodies of a class implementing an interface with static members.
//! Kotlin 2.5's stdlib reaches the same rule through `EmptySequence` and `Sequence.of`, which the
//! shared `stringReplaceCharSequence` box regression covers on every backend.

use super::{assert_official_dependency_sources_produced_with, common, DependencyConsumer};

/// A library-private object implementing an interface inherits its default, never the
/// interface's companion-block statics, which nothing calls.
#[test]
fn dependency_objects_inherit_no_interface_static_members() {
    if std::env::var_os("KRUSTY_KOTLIN_NATIVE").is_none() {
        assert!(
            std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "Native distribution required"
        );
        return;
    }
    // Companion blocks are a language feature from 2.4.20 on.
    if krusty::kotlin_version::environment_release()
        < krusty::kotlin_version::KotlinVersion::V2_4_20
    {
        return;
    }
    let library = include_str!("../fixtures/klib_closure_frames/InterfaceStaticMembers.kt");
    let consumer =
        include_str!("../fixtures/klib_closure_frames/InterfaceStaticMembersConsumer.kt");
    // A release declares one spelling of the feature: 2.5 split `CompanionBlocks` off the
    // combined `CompanionBlocksAndExtensions` earlier releases declare.
    let features = krusty::features::LangFeatures::from_environment();
    let language = if features.table().get("CompanionBlocks").is_some() {
        "-XXLanguage:+CompanionBlocks"
    } else {
        "-XXLanguage:+CompanionBlocksAndExtensions"
    };
    let dependency = common::kotlinc_lib::kotlinc_lib_out_with(
        &[("Library.kt", library)],
        &[language.to_owned()],
    )
    .expect("reference compiler required for the dependency consumer oracle");
    assert_eq!(
        common::kotlinc_box_files_result_with_classpath_and_args(
            &[("Consumer.kt", consumer)],
            "fixture.ConsumerKt",
            &[dependency],
            // Before 2.5 the companion-block dependency is a pre-release declaration.
            &["-Xskip-prerelease-check"],
        ),
        "OK"
    );
    assert_official_dependency_sources_produced_with(
        library,
        &[language],
        &[("Consumer", consumer)],
        "DependencyInterfaceStaticMembers",
        &DependencyConsumer::Native,
    );
}
