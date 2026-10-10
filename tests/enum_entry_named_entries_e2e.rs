//! `ForbidEnumEntryNamedEntries`: an enum entry named `entries` clashes with the synthetic
//! `Enum.entries` property. kotlinc reports the entry as an error with the feature (the default)
//! and as a warning naming the issue without it. Without the feature, `entries` on such an enum is
//! ambiguous between the entry and the property under `PrioritizedEnumEntries`, and names the
//! entry without either feature. Each fixture is compiled by both compilers; the complete located
//! diagnostics (location, every message line, count and order) must be the expected ones for
//! kotlinc and identical for krusty.

use super::common;

/// The fixture after its first line, which selects the language settings.
const CLASH: &str = "enum class E {
    values,
    entries;
    fun inside() = entries
}
fun reference(): Any = E::entries
fun qualified() = E.entries
";

const DECLARATION: &str =
    "Main.kt:4:5: conflicting declarations: the enum entry 'entries' and the property \
     'Enum.entries' (KT-48872).";

fn ambiguity(position: &str) -> [String; 3] {
    [
        format!("Main.kt:{position}: overload resolution ambiguity between candidates:"),
        "| enum entry entries: E".to_string(),
        "| companion val entries: EnumEntries<E>".to_string(),
    ]
}

/// Both compilers' complete errors for the one-file fixture `source`, kotlinc receiving the
/// fixture's `// LANGUAGE:` directive as arguments, must be `expected`.
fn assert_errors(source: &str, expected: &[String]) {
    let sources = [("Main.kt", source)];
    let arguments = common::language_directives::kotlinc_args(source);
    assert_eq!(
        common::reference_error_blocks(&sources, &arguments),
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(
        common::krusty_error_blocks_with_args(&sources, &arguments),
        expected
    );
}

#[test]
fn an_entry_named_entries_is_an_error_with_the_feature() {
    assert_errors(
        &format!("// default language settings\n{CLASH}"),
        &[DECLARATION.to_string()],
    );
}

/// Without the feature the entry and `Enum.entries` are equal candidates for `entries`, unqualified
/// in the enum's body and qualified by the enum; a callable reference still names the property.
#[test]
fn entries_is_ambiguous_without_the_feature() {
    assert_errors(
        &format!("// LANGUAGE: -ForbidEnumEntryNamedEntries\n{CLASH}"),
        &[ambiguity("5:20"), ambiguity("8:21")].concat(),
    );
}

/// Without either feature the entry is a warning naming the deprecation, `E.entries` reads the
/// entry and `E::entries` refers to the property.
#[test]
fn an_entry_named_entries_is_a_warning_without_either_feature() {
    let source = "// LANGUAGE: -PrioritizedEnumEntries -ForbidEnumEntryNamedEntries
enum class E {
    values,
    entries,
    valueOf;
}
fun box(): String {
    val reference = E::entries
    if (reference().toString() != \"[values, entries, valueOf]\") return \"Fail: ${reference()}\"
    if (E.entries.toString() != \"entries\") return \"Fail: ${E.entries}\"
    return \"OK\"
}
";
    let sources = [("Main.kt", source)];
    let arguments = common::language_directives::kotlinc_args(source);
    let result = common::compiler_diagnostics_with_reference_args(&sources, &[], &arguments);
    assert_eq!(result.reference_code, 0, "{}", result.reference_stderr);
    assert_eq!(
        result.krusty_code, 0,
        "{}{}",
        result.krusty_stdout, result.krusty_stderr
    );
    let expected = [format!(
        "{DECLARATION} This will become an error in language version 2.2. See \
         https://youtrack.jetbrains.com/issue/KT-72829."
    )];
    assert_eq!(
        common::warning_blocks(&result.reference_stderr, true),
        expected,
        "kotlinc {}",
        krusty::kotlin_version::target()
    );
    assert_eq!(
        common::warning_blocks(&result.krusty_stderr, false),
        expected
    );
    common::expect_box_same_as_kotlinc(source, "EnumEntryNamedEntries");
}
