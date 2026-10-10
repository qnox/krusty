//! kotlinc's `deprecationError` diagnostics: an error once their language feature is enabled and,
//! until then, a warning that says when it becomes one (`toDeprecationWarningMessage`).

use std::sync::Arc;

use super::LangFeatures;

impl LangFeatures {
    /// The severity of a diagnostic kotlinc declares as a deprecation of the feature `name`. `name`
    /// must be a feature of the reference release's table.
    pub fn deprecation(&self, name: &str) -> DeprecationGate {
        if self.has(name) {
            return DeprecationGate::default();
        }
        let feature = self
            .table()
            .get(name)
            .unwrap_or_else(|| panic!("{name} is not a kotlinc {} feature", self.release()));
        // `appendDeprecationWarningSuffix`.
        let mut suffix = String::from("This will become an error ");
        match feature.since_version {
            Some(since) => suffix.push_str(&format!("in language version {since}")),
            None => suffix.push_str("in a future release"),
        }
        suffix.push('.');
        if let Some(issue) = &feature.issue {
            suffix.push_str(&format!(
                " See https://youtrack.jetbrains.com/issue/{issue}."
            ));
        }
        DeprecationGate {
            warning_suffix: Some(suffix.into()),
        }
    }
}

/// A deprecated construct's severity under one file's settings: an error, or a warning carrying
/// the sentence kotlinc appends. The default is an error.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeprecationGate {
    warning_suffix: Option<Arc<str>>,
}

impl DeprecationGate {
    pub fn is_error(&self) -> bool {
        self.warning_suffix.is_none()
    }

    /// The diagnostic's message for `base`, kotlinc's message of the error.
    pub fn message(&self, base: &str) -> String {
        let Some(suffix) = &self.warning_suffix else {
            return base.to_string();
        };
        let separator = if base.ends_with('.') {
            " "
        } else if base.ends_with(char::is_whitespace) {
            ""
        } else {
            ". "
        };
        format!("{base}{separator}{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kotlin_version::KotlinVersion;
    use crate::language_version::LanguageVersion;

    #[test]
    fn a_disabled_deprecation_warns_when_it_becomes_an_error() {
        let mut settings = LangFeatures::for_release(
            KotlinVersion::V2_4_20,
            LanguageVersion::V2_4,
            LanguageVersion::V2_4,
        );
        assert!(settings
            .deprecation("ProhibitIntersectionReifiedTypeParameter")
            .is_error());
        settings.disable("ProhibitIntersectionReifiedTypeParameter");
        let gate = settings.deprecation("ProhibitIntersectionReifiedTypeParameter");
        assert!(!gate.is_error());
        assert_eq!(
            gate.message("message."),
            "message. This will become an error in language version 2.3. See \
             https://youtrack.jetbrains.com/issue/KTLC-13."
        );
        assert_eq!(
            gate.message("message"),
            "message. This will become an error in language version 2.3. See \
             https://youtrack.jetbrains.com/issue/KTLC-13."
        );
    }
}
