//! PLATFORM: Native stdlib declaration evidence and Kotlin/Native diagnostics.

use crate::common::{self, CompilerOutcome};
use krusty::compilation_target::CompilationTarget;
use krusty::kotlin_version::KotlinVersion;

/// An unresolved JVM annotation does not suppress Native's independent full-value parameter
/// check. Both diagnostics, including the annotation's location and their order, match Native.
#[test]
fn native_parameter_checks_consume_the_selected_full_value_representation() {
    assert_native_parameter_errors(
        "// LANGUAGE: +FullValueClasses\n\
         @JvmInline value class V(var x: Int, val y: Int)\n",
        &[
            "V.kt:2:2: unresolved reference 'JvmInline'.",
            "V.kt:2:26: final value class primary constructor must only have final read-only \
             ('val') property parameters.",
        ],
    );
}

/// The same parameter rule applies to a resolved full value class without a JVM annotation.
#[test]
fn native_full_value_mutable_parameter_is_rejected_without_a_jvm_annotation() {
    assert_native_parameter_errors(
        "// LANGUAGE: +FullValueClasses\n\
         value class V(var x: Int, val y: Int)\n",
        &[
            "V.kt:2:15: final value class primary constructor must only have final read-only \
             ('val') property parameters.",
        ],
    );
}

fn assert_native_parameter_errors(source: &str, errors: &[&str]) {
    if krusty::kotlin_version::target() < KotlinVersion::V2_4_20 {
        return;
    }
    let expected = CompilerOutcome {
        accepted: false,
        errors: errors.iter().map(|error| (*error).to_string()).collect(),
    };
    let args = vec!["-XXLanguage:+FullValueClasses".to_string()];
    if let Some(reference) =
        common::reference_outcome(CompilationTarget::Native, &[("V.kt", source)], &args)
    {
        assert_eq!(reference, expected);
    }
    let Some(stdlib) = krusty::toolchain::kotlin_stdlib_klib(CompilationTarget::Native) else {
        assert!(
            std::env::var_os("KRUSTY_KOTLIN_NATIVE").is_none()
                && std::env::var_os("KRUSTY_REQUIRE_KOTLIN_NATIVE").is_none()
                && std::env::var_os("KRUSTY_REQUIRE_KLIB").is_none(),
            "the provisioned Native distribution must contain its stdlib KLIB"
        );
        return;
    };
    let libraries = krusty::klib_libraries::KlibLibraries::open(&[
        krusty::klib_libraries::KlibLibraryInput::platform_stdlib(
            stdlib,
            krusty::klib_libraries::KlibStdlibSemantics::Native,
        ),
    ])
    .expect("open the reference Native stdlib KLIB");
    let diagnostics = common::front_end_diagnostic_records_with_provider(
        source,
        common::with_native_plugins(
            CompilationTarget::Native,
            Box::new(libraries.for_compilation()),
        ),
        &krusty::features::LangFeatures::from_source(source),
    );
    let actual = common::diagnostic_locations(source, &diagnostics)
        .into_iter()
        .map(|located| {
            let (position, message) = located
                .split_once(": error: ")
                .unwrap_or_else(|| panic!("unexpected located diagnostic: {located}"));
            format!("V.kt:{position}: {message}")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        CompilerOutcome {
            accepted: actual.is_empty(),
            errors: actual,
        },
        expected
    );
}
