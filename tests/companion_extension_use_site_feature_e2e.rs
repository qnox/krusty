//! A written `companion fun/val C.name` is named through `C` only where the use site's file has
//! the companion-extension feature enabled, whatever file declared it. kotlinc reports an
//! unresolved reference at a use site that turned the feature off
//! (`compiler/testData/diagnostics/tests/companionExtensions/callingDeserializedCompanionExtensionWithoutLf.kt`).
//!
//! The declaring file and the use site are two files of one module here, which reaches the same
//! package companion-extension facet a dependency's declarations are published through.

use super::common;

const DECLARATION: &str = "// LANGUAGE: +CompanionBlocksAndExtensions\n\
    companion fun String.foo(): Int = 1\n\
    companion val String.bar: Int get() = 2\n";

fn use_site(language: &str) -> String {
    format!(
        "// LANGUAGE: {language}\n\
         fun test() {{\n\
         \x20   String.foo()\n\
         \x20   String.bar\n\
         }}\n"
    )
}

#[test]
fn a_use_site_with_the_feature_on_names_the_companion_extensions() {
    let Some(diagnostics) = common::module_front_end_diagnostics(&[
        ("Declaration.kt", DECLARATION),
        ("UseSite.kt", &use_site("+CompanionBlocksAndExtensions")),
    ]) else {
        return;
    };
    assert_eq!(diagnostics, Vec::<String>::new());
}

#[test]
fn a_use_site_with_the_feature_off_does_not() {
    let Some(diagnostics) = common::module_front_end_diagnostics(&[
        ("Declaration.kt", DECLARATION),
        ("UseSite.kt", &use_site("-CompanionBlocksAndExtensions")),
    ]) else {
        return;
    };
    assert_eq!(
        diagnostics,
        vec![
            "unresolved reference 'foo'.".to_string(),
            "unresolved reference 'bar'.".to_string(),
        ]
    );
}
