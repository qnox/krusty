//! Per-compilation Kotlin language and API settings.
//!
//! These settings are deliberately values, not process globals: a Bazel worker or language-server
//! process may analyze modules at different language/API levels in sequence. The selected compiler
//! release in [`crate::kotlin_version`] is a separate output-compatibility concern.

use crate::features::LangFeatures;
use crate::language_version::LanguageVersion;

/// The finalized standard Kotlin compatibility inputs for one compilation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageSettings {
    pub language_version: LanguageVersion,
    pub api_version: LanguageVersion,
    pub features: LangFeatures,
}

impl LanguageSettings {
    /// Build settings after parsing all version options and ordered feature overrides.
    ///
    /// Kotlin defaults the API level to the selected language level. An explicitly newer API is
    /// invalid before source analysis starts.
    pub fn new(
        language_version: LanguageVersion,
        api_version: Option<LanguageVersion>,
        feature_arguments: &[String],
    ) -> Result<Self, String> {
        let api_version = api_version.unwrap_or(language_version);
        if api_version > language_version {
            return Err(format!(
                "-api-version ({api_version}) cannot be greater than -language-version ({language_version})."
            ));
        }
        let mut features = LangFeatures::for_versions(language_version, api_version);
        for argument in feature_arguments {
            if !features.apply_cli_arg(argument) {
                return Err(format!(
                    "internal language-settings error: unrecognized feature argument {argument:?}"
                ));
            }
        }
        Ok(Self {
            language_version,
            api_version,
            features,
        })
    }
}

impl Default for LanguageSettings {
    fn default() -> Self {
        Self::new(LanguageVersion::default(), None, &[])
            .expect("the default language/API versions are compatible")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_defaults_to_language_and_feature_overrides_apply_after_the_baseline() {
        let settings = LanguageSettings::new(
            LanguageVersion::V2_2,
            None,
            &["-XXLanguage:+ContextParameters".to_owned()],
        )
        .unwrap();
        assert_eq!(settings.api_version, LanguageVersion::V2_2);
        assert!(settings.features.has("ContextParameters"));
    }

    #[test]
    fn api_cannot_be_newer_than_language() {
        assert_eq!(
            LanguageSettings::new(LanguageVersion::V2_2, Some(LanguageVersion::V2_4), &[],),
            Err("-api-version (2.4) cannot be greater than -language-version (2.2).".to_owned())
        );
    }
}
