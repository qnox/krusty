//! kotlinc's command-line surface: which arguments a release accepts ([`catalog`]), how a command
//! line is split into arguments, values and sources ([`argfile`], [`tokenize`]), and which of those
//! arguments krusty implements ([`disposition`]). `cli` applies the parsed values.

pub mod argfile;
pub mod catalog;
pub mod disposition;
pub mod tokenize;

pub use catalog::{ArgumentSpec, Catalog, Origin, ValueKind};
pub use disposition::Disposition;
pub use tokenize::{tokenize, Occurrence, Recorded, Tokenized, Value};

use krusty::kotlin_version::KotlinVersion;

/// The two lifecycle diagnostics kotlinc reports for an explicitly passed argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    /// `DEPRECATED_CLI_ARG`
    Deprecated,
    /// `REMOVED_CLI_ARG`
    Removed,
}

const ENABLES_DEFAULT_FEATURES: &str =
    " It enables the features that are already on by default in every supported language version.";

/// kotlinc's `checkArgumentsLifecycle`: a warning for every explicit argument that is deprecated
/// or removed as of the reference release, in first-occurrence order.
pub fn lifecycle_warnings(
    tokenized: &Tokenized,
    reference: KotlinVersion,
) -> Vec<(Lifecycle, String)> {
    let reached = |version: &Option<String>| {
        version
            .as_deref()
            .and_then(KotlinVersion::parse)
            .is_some_and(|version| version <= reference)
    };
    let mut warnings = Vec::new();
    for (spec, _) in &tokenized.explicit {
        let feature_note = if spec.changes_language_features() {
            ENABLES_DEFAULT_FEATURES
        } else {
            ""
        };
        if reached(&spec.removed_in) {
            warnings.push((
                Lifecycle::Removed,
                format!(
                    "The argument '{}' was removed in Kotlin {}. It has no effect.{feature_note}",
                    spec.name,
                    spec.removed_in.as_deref().unwrap_or_default()
                ),
            ));
        } else if reached(&spec.deprecated_in) {
            let removal = match &spec.removed_in {
                Some(version) => version.clone(),
                None => "one of the future releases".to_string(),
            };
            let note = match &spec.deprecation_message {
                Some(message) => format!(" {message}"),
                None => feature_note.to_string(),
            };
            warnings.push((
                Lifecycle::Deprecated,
                format!(
                    "The argument '{}' is deprecated since Kotlin {}. It will be removed in {removal}.{note}",
                    spec.name,
                    spec.deprecated_in.as_deref().unwrap_or_default()
                ),
            ));
        }
    }
    warnings
}

/// kotlinc's plain-text renderer lowercases a message's first letter unless the message opens with
/// `Java`/`Kotlin` or two capitals.
pub fn render(message: &str) -> String {
    let mut chars = message.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let second_is_upper = chars.next().is_some_and(char::is_uppercase);
    if message.starts_with("Java")
        || message.starts_with("Kotlin")
        || (first.is_uppercase() && second_is_upper)
    {
        return message.to_string();
    }
    let mut rendered = first.to_ascii_lowercase().to_string();
    rendered.push_str(&message[first.len_utf8()..]);
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lifecycle(arguments: &[&str]) -> Vec<(Lifecycle, String)> {
        let catalog = Catalog::for_version(KotlinVersion::V2_4_20).unwrap();
        let tokenized = tokenize(
            catalog,
            arguments
                .iter()
                .map(|argument| argument.to_string())
                .collect(),
            Vec::new(),
        );
        lifecycle_warnings(&tokenized, KotlinVersion::V2_4_20)
    }

    /// kotlinc 2.4.20's words for both lifecycle states (measured).
    #[test]
    fn deprecated_and_removed_arguments_are_warned_in_kotlincs_words() {
        assert_eq!(
            lifecycle(&["-Xjvm-default=all", "-Xuse-k2", "-Xvalue-classes"]),
            vec![
                (
                    Lifecycle::Deprecated,
                    "The argument '-Xjvm-default' is deprecated since Kotlin 2.2.0. It will be removed in one of the future releases. Use `-jvm-default` instead.".to_string()
                ),
                (
                    Lifecycle::Removed,
                    "The argument '-Xuse-k2' was removed in Kotlin 2.2.0. It has no effect.".to_string()
                ),
                (
                    Lifecycle::Removed,
                    "The argument '-Xvalue-classes' was removed in Kotlin 2.4.20. It has no effect.".to_string()
                ),
            ]
        );
    }

    #[test]
    fn rendering_lowercases_like_kotlinc() {
        assert_eq!(render("Invalid argument: -foo"), "invalid argument: -foo");
        assert_eq!(render("Kotlin home"), "Kotlin home");
        assert_eq!(render("ATTENTION!"), "ATTENTION!");
    }
}
